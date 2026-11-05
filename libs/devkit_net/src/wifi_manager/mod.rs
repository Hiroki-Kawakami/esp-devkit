/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * Saved networks, joining them without being asked, and the Wi-Fi on/off
 * switch, over `Wifi`.
 */

use alloc::boxed::Box;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::cmp::Reverse;
use core::ffi::CStr;
use core::future::{Future, pending, poll_fn};
use core::pin::pin;
use core::task::Poll;
use core::time::Duration;

use idf::executor::{self, JoinHandle, Spawner};
use idf::log::{info, warn};
use idf::sync::{Mutex, OnceLock};
use idf::time::Instant;

use crate::watch::Watch;
use crate::wifi::{Auth, ConnectError, DisconnectReason, ScanRecord, Security, StaConfig, Wifi, WifiState, offload};
use crate::{Error, Result};

pub(crate) mod store;

use store::{Cipher, SLOTS, Saved, Write};

const TAG: &CStr = c"wifi_manager";

const JOIN_TIMEOUT: Duration = Duration::from_secs(20);
const BACKOFF_FIRST: Duration = Duration::from_secs(10);
const BACKOFF_MAX: Duration = Duration::from_secs(300);
const RSSI_PERIOD: Duration = Duration::from_secs(5);
const WEAK_RSSI: i8 = -80;
const LAST_JOINED_BONUS: i16 = 10;
const AUTH_FAILURES_TO_SKIP: u8 = 2;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum State {
    /// Wi-Fi is off; the radio is stopped.
    Disabled,
    /// On, not connected, and not trying: nothing saved, or `disconnect` was
    /// called.
    Idle,
    Scanning,
    Joining { ssid: Vec<u8> },
    /// `ssid` is empty when joined through `Wifi` instead.
    Connected { ssid: Vec<u8>, bssid: [u8; 6], channel: u8, rssi: Option<i8> },
    /// Found nothing to join; scans again at `until`.
    Waiting { until: Instant },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum JoinError {
    Disabled,
    InvalidSsid,
    InvalidPassword,
    NotSaved,
    NotFound,
    AuthFailed,
    Rejected(DisconnectReason),
    Timeout,
    Driver(i32),
}

impl From<ConnectError> for JoinError {
    fn from(err: ConnectError) -> Self {
        match err {
            ConnectError::Stopped => Self::Disabled,
            ConnectError::NotFound => Self::NotFound,
            ConnectError::AuthFailed => Self::AuthFailed,
            ConnectError::Rejected(reason) => Self::Rejected(reason),
            ConnectError::Driver(code) => Self::Driver(code),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScanError {
    Disabled,
    Driver(i32),
}

/// A network a scan saw, by SSID: the strongest access point of it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ScanEntry {
    pub ssid: Vec<u8>,
    pub rssi: i8,
    pub security: Security,
    pub channel: u8,
    pub saved: bool,
    pub connected: bool,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NetworkInfo {
    pub ssid: Vec<u8>,
    pub hidden: bool,
    pub secured: bool,
}

struct Inner {
    slots: [Option<Saved>; SLOTS],
    enabled: bool,
    paused: bool,
    /// Calls that use the radio and wait for the joining loop to step aside.
    preempting: u32,
    /// Of the network joined last through the manager.
    connected: Option<Vec<u8>>,
    auth_failures: Vec<(Vec<u8>, u8)>,
    reset_backoff: bool,
}

impl Inner {
    fn find(&self, ssid: &[u8]) -> Option<usize> {
        self.slots.iter().position(|s| s.as_ref().is_some_and(|s| s.ssid == ssid))
    }

    fn next_seq(&self) -> u32 {
        self.slots.iter().flatten().map(|s| s.seq).max().map_or(0, |seq| seq.wrapping_add(1))
    }

    fn slot_for(&self, ssid: &[u8]) -> usize {
        self.find(ssid)
            .or_else(|| self.slots.iter().position(Option::is_none))
            .unwrap_or_else(|| {
                (0..SLOTS).min_by_key(|&i| self.slots[i].as_ref().map_or(0, |s| s.seq)).unwrap_or(0)
            })
    }

    fn saved(&self) -> Vec<Saved> {
        self.slots.iter().flatten().cloned().collect()
    }

    fn skipped(&self, ssid: &[u8]) -> bool {
        self.auth_failures.iter().any(|(s, n)| s == ssid && *n >= AUTH_FAILURES_TO_SKIP)
    }

    fn note_auth_failure(&mut self, ssid: &[u8]) {
        match self.auth_failures.iter_mut().find(|(s, _)| s == ssid) {
            Some((_, n)) => *n = n.saturating_add(1),
            None => self.auth_failures.push((ssid.to_vec(), 1)),
        }
    }

    fn clear_auth_failures(&mut self, ssid: &[u8]) {
        self.auth_failures.retain(|(s, _)| s != ssid);
    }
}

pub struct Manager {
    wifi: &'static Wifi,
    cipher: Cipher,
    inner: Mutex<Inner>,
    /// Held while one caller uses the radio.
    radio: Mutex<()>,
    /// The last scan and when it finished; locked by whoever scans.
    last_scan: Mutex<Option<(Instant, Vec<ScanRecord>)>>,
    state: Watch<State>,
    kick: Watch<()>,
    cancel: Watch<()>,
}

static MANAGER: OnceLock<&'static Manager> = OnceLock::new();

/// Reads the saved networks and the on/off switch, and joins by itself from
/// then on. Nothing runs before; call once `devkit_net::start` returned.
pub fn start() -> Result<&'static Manager> {
    let net = crate::net::global()?;
    let spawner = net.spawner().ok_or(Error::NotStarted)?;
    let wifi = crate::wifi().ok_or(Error::Unsupported)?;
    if MANAGER.get().is_some() {
        return Err(Error::AlreadyStarted);
    }
    let mac = idf::system::factory_mac().map_err(|err| {
        warn!(TAG, "no factory mac: {err}");
        Error::Unsupported
    })?;
    let manager = load(wifi, mac);
    MANAGER.set(manager).map_err(|_| Error::AlreadyStarted)?;
    spawn(spawner, manager);
    Ok(manager)
}

// This and `load` are out of line, so what they build sits in their own
// frames, one at a time, instead of all in `start`'s.
#[inline(never)]
fn spawn(spawner: &Spawner, manager: &'static Manager) {
    spawner.spawn(manager.run());
}

#[inline(never)]
fn load(wifi: &'static Wifi, mac: [u8; 6]) -> &'static Manager {
    let cipher = Cipher::new(mac);
    let loaded = store::load(&cipher).unwrap_or_else(|err| {
        warn!(TAG, "nvs: {err}; nothing saved");
        store::Loaded { slots: Default::default(), enabled: None }
    });
    let enabled = loaded.enabled.unwrap_or(true);
    Box::leak(Box::new(Manager {
        wifi,
        cipher,
        inner: Mutex::new(Inner {
            slots: loaded.slots,
            enabled,
            paused: false,
            preempting: 0,
            connected: None,
            auth_failures: Vec::new(),
            reset_backoff: false,
        }),
        radio: Mutex::new(()),
        last_scan: Mutex::new(None),
        state: Watch::new(if enabled { State::Idle } else { State::Disabled }),
        kick: Watch::new(()),
        cancel: Watch::new(()),
    }))
}

pub fn get() -> Option<&'static Manager> {
    MANAGER.get().copied()
}

fn valid_password(password: &str) -> bool {
    let b = password.as_bytes();
    b.is_empty()
        || ((8..=63).contains(&b.len()) && b.iter().all(|c| (0x20..=0x7e).contains(c)))
        || (b.len() == 64 && b.iter().all(u8::is_ascii_hexdigit))
}

fn valid_ssid(ssid: &[u8]) -> bool {
    (1..=32).contains(&ssid.len())
}

fn sta_config<'a>(saved: &'a Saved, channel: Option<u8>) -> StaConfig<'a> {
    let auth = if saved.password.is_empty() { Auth::Open } else { Auth::Psk(&saved.password) };
    StaConfig { ssid: &saved.ssid, auth, bssid: None, channel }
}

/// Saved networks to try, best first, with the channel a scan saw them on.
pub(crate) fn rank<'a>(
    records: &[ScanRecord],
    saved: &'a [Saved],
    skip: impl Fn(&[u8]) -> bool,
) -> Vec<(&'a Saved, Option<u8>)> {
    let last = saved.iter().max_by_key(|s| s.seq).map(|s| s.ssid.as_slice());
    let mut seen = Vec::new();
    let mut unseen = Vec::new();
    for s in saved.iter().filter(|s| !skip(&s.ssid)) {
        match records.iter().filter(|r| r.ssid == s.ssid).max_by_key(|r| r.rssi) {
            Some(r) => {
                let bonus = if Some(s.ssid.as_slice()) == last { LAST_JOINED_BONUS } else { 0 };
                seen.push((s, r.channel, r.rssi < WEAK_RSSI, i16::from(r.rssi) + bonus));
            }
            None if s.hidden => unseen.push(s),
            None => {}
        }
    }
    seen.sort_unstable_by_key(|&(_, _, weak, score)| (weak, Reverse(score)));
    unseen.sort_unstable_by_key(|s| Reverse(s.seq));
    seen.into_iter().map(|(s, channel, ..)| (s, Some(channel))).chain(unseen.into_iter().map(|s| (s, None))).collect()
}

pub(crate) fn entries(records: &[ScanRecord], saved: &[Saved], connected: Option<&[u8]>) -> Vec<ScanEntry> {
    let mut out: Vec<ScanEntry> = Vec::new();
    for r in records.iter().filter(|r| !r.ssid.is_empty()) {
        if let Some(e) = out.iter_mut().find(|e| e.ssid == r.ssid) {
            if r.rssi > e.rssi {
                (e.rssi, e.security, e.channel) = (r.rssi, r.security, r.channel);
            }
            continue;
        }
        out.push(ScanEntry {
            ssid: r.ssid.clone(),
            rssi: r.rssi,
            security: r.security,
            channel: r.channel,
            saved: saved.iter().any(|s| s.ssid == r.ssid),
            connected: connected == Some(r.ssid.as_slice()),
        });
    }
    out.sort_by_cached_key(|e| (Reverse(e.connected), Reverse(e.saved), Reverse(e.rssi)));
    out
}

/// While held, the joining loop leaves the radio alone.
struct Preempt<'a>(&'a Manager);

impl Drop for Preempt<'_> {
    fn drop(&mut self) {
        self.0.inner.lock().preempting -= 1;
        self.0.kick.set(());
    }
}

/// Leaves the network if a join is dropped halfway.
struct AbortJoin {
    wifi: Option<&'static Wifi>,
}

impl Drop for AbortJoin {
    fn drop(&mut self) {
        let Some(wifi) = self.wifi.take() else { return };
        if let Some(spawner) = crate::net::registry().spawner() {
            spawner.spawn(async move {
                let _ = wifi.disconnect().await;
            });
        }
    }
}

async fn next_change<T: Clone>(watch: &Watch<T>, seen: u32) {
    watch.after(seen).await;
}

async fn until<F: Future<Output = ()>>(f: F, stop: impl Future<Output = ()>) {
    let mut f = pin!(f);
    let mut stop = pin!(stop);
    poll_fn(|cx| {
        if f.as_mut().poll(cx).is_ready() || stop.as_mut().poll(cx).is_ready() {
            return Poll::Ready(());
        }
        Poll::Pending
    })
    .await
}

impl Manager {
    pub fn state(&self) -> State {
        self.state.get().0
    }

    /// The next state after this call.
    pub async fn changed(&self) -> State {
        let (_, seen) = self.state.get();
        self.state.after(seen).await.0
    }

    pub fn enabled(&self) -> bool {
        self.inner.lock().enabled
    }

    /// Turns Wi-Fi on or off and keeps the choice in NVS. Off leaves the
    /// network and stops the radio; on starts it and joins by itself again.
    pub async fn set_enabled(&self, enabled: bool) -> idf::Result<()> {
        store::save_enabled(enabled)?;
        {
            let mut inner = self.inner.lock();
            inner.enabled = enabled;
            inner.paused = false;
            inner.reset_backoff = true;
        }
        let _preempt = self.preempt();
        let _radio = self.radio.lock_async().await;
        if enabled != self.enabled() {
            return Ok(());
        }
        let result = if enabled { self.wifi.start().await } else { self.wifi.stop().await };
        if let Err(err) = result {
            warn!(TAG, "radio {}: {err:?}", if enabled { "start" } else { "stop" });
        }
        if !enabled {
            self.inner.lock().connected = None;
            self.set_state(State::Disabled);
        }
        info!(TAG, "wifi {}", if enabled { "on" } else { "off" });
        Ok(())
    }

    /// Saved networks, joined most recently first. Passwords stay inside.
    pub fn networks(&self) -> Vec<NetworkInfo> {
        let mut saved = self.inner.lock().saved();
        saved.sort_by_cached_key(|s| Reverse(s.seq));
        saved
            .into_iter()
            .map(|s| NetworkInfo { ssid: s.ssid, hidden: s.hidden, secured: !s.password.is_empty() })
            .collect()
    }

    /// Joins a network and saves it once joined; an empty `password` means an
    /// open network. A `hidden` network is joined even when no scan sees it.
    /// Dropping the future gives up on the join; on failure the manager goes
    /// back to joining saved networks by itself.
    pub async fn join(&self, ssid: &[u8], password: &str, hidden: bool) -> core::result::Result<(), JoinError> {
        if !valid_ssid(ssid) {
            return Err(JoinError::InvalidSsid);
        }
        if !valid_password(password) {
            return Err(JoinError::InvalidPassword);
        }
        self.join_with(Saved { ssid: ssid.to_vec(), password: String::from(password), hidden, seq: 0 }).await
    }

    /// Joins a saved network with what was saved for it.
    pub async fn join_saved(&self, ssid: &[u8]) -> core::result::Result<(), JoinError> {
        let saved = {
            let inner = self.inner.lock();
            inner.find(ssid).and_then(|i| inner.slots[i].clone())
        };
        self.join_with(saved.ok_or(JoinError::NotSaved)?).await
    }

    async fn join_with(&self, saved: Saved) -> core::result::Result<(), JoinError> {
        {
            let mut inner = self.inner.lock();
            if !inner.enabled {
                return Err(JoinError::Disabled);
            }
            inner.paused = false;
            inner.clear_auth_failures(&saved.ssid);
        }
        let _preempt = self.preempt();
        let _radio = self.radio.lock_async().await;
        if !self.enabled() {
            return Err(JoinError::Disabled);
        }
        self.set_state(State::Joining { ssid: saved.ssid.clone() });
        let mut abort = AbortJoin { wifi: Some(self.wifi) };
        let result = executor::timeout(JOIN_TIMEOUT, self.wifi.connect(&sta_config(&saved, None))).await;
        abort.wifi = None;
        let result = match result {
            Ok(result) => result.map_err(JoinError::from),
            Err(_) => {
                let _ = self.wifi.disconnect().await;
                Err(JoinError::Timeout)
            }
        };
        match result {
            Ok(()) => {
                info!(TAG, "joined {}", String::from_utf8_lossy(&saved.ssid));
                if let Err(err) = self.remember(saved).apply() {
                    warn!(TAG, "not saved: {err}");
                }
            }
            Err(err) => warn!(TAG, "join {}: {err:?}", String::from_utf8_lossy(&saved.ssid)),
        }
        result
    }

    /// Saves without joining; replaces what was saved for the SSID.
    pub fn add(&self, ssid: &[u8], password: &str, hidden: bool) -> idf::Result<()> {
        if !valid_ssid(ssid) || !valid_password(password) {
            return Err(idf::Error::INVALID_ARG);
        }
        let write = {
            let mut inner = self.inner.lock();
            let slot = inner.slot_for(ssid);
            let seq = inner.next_seq();
            let saved = Saved { ssid: ssid.to_vec(), password: String::from(password), hidden, seq };
            inner.clear_auth_failures(ssid);
            let write = Write::put(&self.cipher, slot, &saved);
            inner.slots[slot] = Some(saved);
            write
        };
        write.apply()?;
        self.kick.set(());
        Ok(())
    }

    /// Erases a saved network, leaving it when connected to it. `false` when
    /// it was not saved.
    pub async fn forget(&self, ssid: &[u8]) -> idf::Result<bool> {
        let (slot, current) = {
            let mut inner = self.inner.lock();
            let Some(slot) = inner.find(ssid) else { return Ok(false) };
            inner.slots[slot] = None;
            inner.clear_auth_failures(ssid);
            (slot, inner.connected.as_deref() == Some(ssid))
        };
        Write::erase(slot).apply()?;
        if current {
            let _preempt = self.preempt();
            let _radio = self.radio.lock_async().await;
            self.inner.lock().connected = None;
            let _ = self.wifi.disconnect().await;
        }
        self.kick.set(());
        Ok(true)
    }

    /// Leaves the network and stops joining by itself until `join`,
    /// `join_saved`, `reconnect`, or Wi-Fi turned on again.
    pub async fn disconnect(&self) {
        self.inner.lock().paused = true;
        let _preempt = self.preempt();
        let _radio = self.radio.lock_async().await;
        self.inner.lock().connected = None;
        let _ = self.wifi.disconnect().await;
        if self.enabled() {
            self.set_state(State::Idle);
        }
    }

    /// Joins saved networks by itself again, right away, including ones a
    /// wrong password had it skip.
    pub fn reconnect(&self) {
        {
            let mut inner = self.inner.lock();
            inner.paused = false;
            inner.auth_failures.clear();
            inner.reset_backoff = true;
        }
        self.cancel.set(());
        self.kick.set(());
    }

    /// One scan, shared with whoever else asked while it ran.
    pub async fn scan(&self) -> core::result::Result<Vec<ScanEntry>, ScanError> {
        let records = self.scan_records().await?;
        let inner = self.inner.lock();
        Ok(entries(&records, &inner.saved(), self.connected_ssid(&inner)))
    }

    /// Scans every `interval` while the handle lives, starting at once.
    pub fn watch_scan(&'static self, interval: Duration) -> ScanWatch {
        let results = Arc::new(Watch::new(Ok(Vec::new())));
        let out = results.clone();
        let task = crate::net::registry().spawner().map(|spawner| {
            spawner.spawn(async move {
                loop {
                    out.set(self.scan().await);
                    executor::sleep(interval).await;
                }
            })
        });
        ScanWatch { results, seen: 0, task }
    }

    fn connected_ssid<'a>(&self, inner: &'a Inner) -> Option<&'a [u8]> {
        match self.state.get().0 {
            State::Connected { .. } => inner.connected.as_deref(),
            _ => None,
        }
    }

    fn set_state(&self, state: State) {
        self.state.update(|now| (*now != state).then_some(state));
    }

    fn preempt(&self) -> Preempt<'_> {
        self.inner.lock().preempting += 1;
        self.cancel.set(());
        Preempt(self)
    }

    /// Puts `saved` in its slot as joined last.
    fn remember(&self, mut saved: Saved) -> Write {
        let mut inner = self.inner.lock();
        let slot = inner.slot_for(&saved.ssid);
        saved.seq = inner.next_seq();
        inner.clear_auth_failures(&saved.ssid);
        inner.connected = Some(saved.ssid.clone());
        let write = Write::put(&self.cipher, slot, &saved);
        inner.slots[slot] = Some(saved);
        write
    }

    async fn scan_records(&self) -> core::result::Result<Vec<ScanRecord>, ScanError> {
        let asked = Instant::now();
        let mut last = self.last_scan.lock_async().await;
        if let Some((done, records)) = &*last
            && *done >= asked
        {
            return Ok(records.clone());
        }
        let records = {
            let _radio = self.radio.lock_async().await;
            self.wifi.scan().await.map_err(|err| match err {
                ConnectError::Stopped => ScanError::Disabled,
                ConnectError::Driver(code) => ScanError::Driver(code),
                _ => ScanError::Driver(0),
            })?
        };
        *last = Some((Instant::now(), records.clone()));
        Ok(records)
    }

    /// The joining loop, on the stack's task.
    async fn run(&'static self) {
        let mut backoff = BACKOFF_FIRST;
        let mut retry = None;
        loop {
            let (_, cancelled) = self.cancel.get();
            until(self.step(&mut backoff, &mut retry), next_change(&self.cancel, cancelled)).await;
        }
    }

    async fn step(&self, backoff: &mut Duration, retry: &mut Option<Vec<u8>>) {
        let (_, kicked) = self.kick.get();
        let wait_kick = || next_change(&self.kick, kicked);
        let (enabled, paused, preempted) = {
            let mut inner = self.inner.lock();
            if core::mem::take(&mut inner.reset_backoff) {
                *backoff = BACKOFF_FIRST;
            }
            (inner.enabled, inner.paused, inner.preempting > 0)
        };
        if preempted {
            return wait_kick().await;
        }
        if !enabled {
            self.set_state(State::Disabled);
            return wait_kick().await;
        }
        let (wifi_state, wifi_seen) = self.wifi.watch().get();
        match wifi_state {
            WifiState::Stopped => {
                let started = {
                    let _radio = self.radio.lock_async().await;
                    self.wifi.start().await
                };
                if let Err(err) = started {
                    warn!(TAG, "radio start: {err:?}");
                    self.wait(backoff).await;
                }
            }
            WifiState::Connecting => next_change(self.wifi.watch(), wifi_seen).await,
            WifiState::Connected { bssid, channel } => {
                *backoff = BACKOFF_FIRST;
                let ssid = self.inner.lock().connected.clone().unwrap_or_default();
                let rssi = self.wifi.rssi().await;
                self.set_state(State::Connected { ssid: ssid.clone(), bssid, channel, rssi });
                until(next_change(self.wifi.watch(), wifi_seen), executor::sleep(RSSI_PERIOD)).await;
                if let WifiState::Disconnected(Some(DisconnectReason::Lost)) = self.wifi.state()
                    && !ssid.is_empty()
                {
                    info!(TAG, "lost {}", String::from_utf8_lossy(&ssid));
                    *retry = Some(ssid);
                }
            }
            WifiState::Disconnected(_) => {
                let saved = self.inner.lock().saved();
                if paused || saved.is_empty() {
                    self.set_state(State::Idle);
                    return wait_kick().await;
                }
                if let Some(ssid) = retry.take()
                    && let Some(s) = saved.iter().find(|s| s.ssid == ssid)
                    && self.attempt(s, None).await
                {
                    return;
                }
                self.set_state(State::Scanning);
                let records = match self.scan_records().await {
                    Ok(records) => records,
                    Err(err) => {
                        warn!(TAG, "scan: {err:?}");
                        return self.wait(backoff).await;
                    }
                };
                let candidates = {
                    let inner = self.inner.lock();
                    rank(&records, &saved, |ssid| inner.skipped(ssid))
                        .into_iter()
                        .map(|(s, channel)| (s.clone(), channel))
                        .collect::<Vec<_>>()
                };
                for (s, channel) in &candidates {
                    if self.attempt(s, *channel).await {
                        return;
                    }
                }
                self.wait(backoff).await;
            }
        }
    }

    /// Sleeps `backoff` as `Waiting`, then doubles it. A kick ends it early.
    async fn wait(&self, backoff: &mut Duration) {
        let (_, kicked) = self.kick.get();
        self.set_state(State::Waiting { until: Instant::now() + *backoff });
        until(executor::sleep(*backoff), next_change(&self.kick, kicked)).await;
        *backoff = (*backoff * 2).min(BACKOFF_MAX);
    }

    /// Joins a saved network for the loop; `true` once joined.
    async fn attempt(&self, saved: &Saved, channel: Option<u8>) -> bool {
        self.set_state(State::Joining { ssid: saved.ssid.clone() });
        let result = {
            let _radio = self.radio.lock_async().await;
            match executor::timeout(JOIN_TIMEOUT, self.wifi.connect(&sta_config(saved, channel))).await {
                Ok(result) => result.map_err(JoinError::from),
                Err(_) => {
                    let _ = self.wifi.disconnect().await;
                    Err(JoinError::Timeout)
                }
            }
        };
        let ssid = String::from_utf8_lossy(&saved.ssid).into_owned();
        match result {
            Ok(()) => {
                info!(TAG, "joined {ssid}");
                let write = self.remember(saved.clone());
                let _ = offload("wifi_mgr_save", move || write.apply()).await;
                true
            }
            Err(err) => {
                warn!(TAG, "join {ssid}: {err:?}");
                if err == JoinError::AuthFailed {
                    self.inner.lock().note_auth_failure(&saved.ssid);
                }
                false
            }
        }
    }
}

/// Scan results as they come; scanning stops when dropped.
pub struct ScanWatch {
    results: Arc<Watch<core::result::Result<Vec<ScanEntry>, ScanError>>>,
    seen: u32,
    task: Option<JoinHandle<()>>,
}

impl ScanWatch {
    /// The next result after the one returned last.
    pub async fn changed(&mut self) -> core::result::Result<Vec<ScanEntry>, ScanError> {
        if self.task.is_none() {
            return pending().await;
        }
        let (result, seen) = self.results.after(self.seen).await;
        self.seen = seen;
        result
    }
}

impl Drop for ScanWatch {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
