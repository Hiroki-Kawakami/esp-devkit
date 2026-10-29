/*
 * SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Hiroki Kawakami
 */

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;

use idf::Result;
use idf::sync::Mutex;
use sim_device::Host;

use crate::touch::{self, Driver, Point, Touch};

pub(crate) struct SimTouch {
    latest: Arc<Mutex<Vec<Point>>>,
    points: Vec<Point>,
}

impl Driver for SimTouch {
    fn poll(&mut self) -> Result<&[Point]> {
        self.points.clone_from(&self.latest.lock());
        Ok(&self.points)
    }
}

pub(crate) fn start(host: &'static Host, config: touch::Config) -> Result<&'static Touch> {
    let latest = Arc::new(Mutex::try_new(Vec::new())?);
    let touch = touch::start(SimTouch { latest: latest.clone(), points: Vec::new() }, config)?;
    host.set_touch_listener(Box::new(move |points| {
        let mut l = latest.lock();
        l.clear();
        l.extend(points.iter().map(|p| Point { x: p.x, y: p.y, id: p.id }));
        drop(l);
        touch.interrupt();
    }));
    Ok(touch)
}
