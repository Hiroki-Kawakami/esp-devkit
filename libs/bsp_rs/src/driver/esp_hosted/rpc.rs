/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 *
 * RPC client. One request is in flight at a time; a response whose uid does
 * not match the pending request is dropped instead of being handed to the
 * next caller.
 */

use alloc::vec::Vec;
use core::ffi::CStr;
use core::ops::Range;
use core::sync::atomic::{AtomicU32, Ordering};
use core::time::Duration;

use idf::log::{error, warn};
use idf::sync::{Mutex, Semaphore};
use idf::{Error, Result};

use super::proto::pb::{Encode, Message};
use super::proto::{Rpc, RpcId, RpcPayload, RpcType};
use super::transport::Transport;
use super::wire::{self, If};

const TAG: &CStr = c"esp_hosted_rpc";

pub(super) const TIMEOUT: Duration = Duration::from_secs(5);

const TLV_HEADER_LEN: usize = 3 + wire::SERIAL_EP_RSP.len() + 3;

pub(super) struct Response {
    data: Vec<u8>,
    msg: Range<usize>,
}

impl Response {
    fn rpc(&self) -> Result<Rpc<'_>> {
        Rpc::decode(&self.data[self.msg.clone()]).map_err(|_| Error::FAIL)
    }
}

struct RxState {
    buf: Vec<u8>,
    overflow: bool,
}

pub(super) struct Client {
    call_lock: Mutex<u32>,
    done: Semaphore,
    pending_uid: AtomicU32,
    response: Mutex<Option<Response>>,
    rx: Mutex<RxState>,
}

impl Client {
    pub(super) fn new() -> Result<Self> {
        Ok(Self {
            call_lock: Mutex::new(0),
            done: Semaphore::try_new(0, 1)?,
            pending_uid: AtomicU32::new(0),
            response: Mutex::new(None),
            rx: Mutex::new(RxState { buf: Vec::new(), overflow: false }),
        })
    }

    fn send_serial(transport: &Transport, buf: &mut [u8]) -> Result<()> {
        if buf.len() > wire::MAX_SERIAL {
            return Err(Error::INVALID_SIZE);
        }
        let len = buf.len() - TLV_HEADER_LEN;
        let ep = wire::SERIAL_EP_RSP;
        buf[0] = wire::SERIAL_TLV_EP;
        buf[1..3].copy_from_slice(&(ep.len() as u16).to_le_bytes());
        buf[3..3 + ep.len()].copy_from_slice(ep);
        let pos = 3 + ep.len();
        buf[pos] = wire::SERIAL_TLV_DATA;
        buf[pos + 1..pos + 3].copy_from_slice(&(len as u16).to_le_bytes());

        let mut chunks = buf.chunks(wire::MAX_PAYLOAD).peekable();
        while let Some(chunk) = chunks.next() {
            let flags = if chunks.peek().is_some() { wire::FLAG_MORE_FRAGMENT } else { 0 };
            transport.send(If::Serial, flags, chunk, None)?;
        }
        Ok(())
    }

    /// `f` picks the result out of the response payload; `None` there is
    /// `FAIL`. `NOT_SUPPORTED` when the coprocessor does not implement the
    /// request.
    pub(super) fn call<T>(
        &self,
        transport: &Transport,
        id: RpcId,
        payload: Option<RpcPayload<'_>>,
        timeout: Duration,
        f: impl FnOnce(&RpcPayload<'_>) -> Option<Result<T>>,
    ) -> Result<T> {
        let mut uid = self.call_lock.lock();
        *uid = uid.wrapping_add(1).max(1);
        let req = Rpc { msg_type: RpcType::Req, msg_id: id, uid: *uid, payload };
        let mut buf = alloc::vec![0; TLV_HEADER_LEN + req.encoded_len()];
        req.encode(&mut buf[TLV_HEADER_LEN..]);

        self.done.try_take();
        *self.response.lock() = None;
        self.pending_uid.store(*uid, Ordering::Release);
        let sent = Self::send_serial(transport, &mut buf);
        let response = sent.and_then(|()| {
            if self.done.take_timeout(timeout) {
                self.response.lock().take().ok_or(Error::FAIL)
            } else {
                error!(TAG, "no response to request {}", id.0);
                Err(Error::TIMEOUT)
            }
        });
        self.pending_uid.store(0, Ordering::Release);
        drop(uid);

        let response = response?;
        let rpc = response.rpc()?;
        if rpc.msg_id == RpcId::Resp_Base {
            return Err(Error::NOT_SUPPORTED);
        }
        rpc.payload.as_ref().and_then(f).unwrap_or(Err(Error::FAIL))
    }

    fn deliver(&self, data: Vec<u8>, on_event: impl FnOnce(&Rpc<'_>)) -> Option<Vec<u8>> {
        let p = &data;
        if p.len() < TLV_HEADER_LEN || p[0] != wire::SERIAL_TLV_EP {
            return Some(data);
        }
        let pos = 3 + usize::from(u16::from_le_bytes([p[1], p[2]]));
        if pos + 3 > p.len() || p[pos] != wire::SERIAL_TLV_DATA {
            return Some(data);
        }
        let len = usize::from(u16::from_le_bytes([p[pos + 1], p[pos + 2]]));
        let msg = pos + 3..pos + 3 + len;
        if len == 0 || msg.end > p.len() {
            return Some(data);
        }
        let Ok(rpc) = Rpc::decode(&p[msg.clone()]) else {
            warn!(TAG, "undecodable message dropped");
            return Some(data);
        };
        if rpc.msg_type == RpcType::Event {
            on_event(&rpc);
            return Some(data);
        }
        if rpc.msg_type == RpcType::Resp && rpc.uid != 0 && rpc.uid == self.pending_uid.load(Ordering::Acquire) {
            self.pending_uid.store(0, Ordering::Release);
            *self.response.lock() = Some(Response { data, msg });
            self.done.give();
            return None;
        }
        warn!(TAG, "stale response {} dropped", rpc.msg_id.0);
        Some(data)
    }

    /// On the transport rx task.
    pub(super) fn on_serial(&self, payload: &[u8], flags: u8, on_event: impl FnOnce(&Rpc<'_>)) {
        let mut rx = self.rx.lock();
        if !rx.overflow {
            if rx.buf.len() + payload.len() > wire::MAX_SERIAL {
                rx.overflow = true;
            } else {
                rx.buf.extend_from_slice(payload);
            }
        }
        if flags & wire::FLAG_MORE_FRAGMENT != 0 {
            return;
        }
        if rx.overflow {
            warn!(TAG, "rpc message over {} bytes dropped", wire::MAX_SERIAL);
            rx.buf.clear();
        } else {
            let data = core::mem::take(&mut rx.buf);
            if let Some(mut data) = self.deliver(data, on_event) {
                data.clear();
                rx.buf = data;
            }
        }
        rx.overflow = false;
    }
}

pub(super) fn status(code: i32) -> Result<()> {
    Error::from_code(code).map_or(Ok(()), Err)
}
