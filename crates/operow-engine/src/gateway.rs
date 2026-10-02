use std::collections::HashMap;

use operow_core::{BusId, CanFrame, EcuConfig, NodeKind, RouteRule};

use crate::ecu::{Ecu, EcuCtx, FrameMeta, PeriodicEcu};

/// Timer ids at or above this value are delayed forwards; lower ids belong
/// to the node's own periodic `tx` messages.
const FORWARD_TIMER_BASE: u32 = 0x8000_0000;

/// An [`Ecu`] that forwards frames between buses according to the routes of
/// a [`NodeKind::Gateway`] and also transmits its own `tx` messages.
pub struct GatewayEcu {
    own: PeriodicEcu,
    routes: Vec<RouteRule>,
    next_timer: u32,
    delayed: HashMap<u32, (FrameMeta, BusId, CanFrame)>,
}

impl GatewayEcu {
    pub fn new(config: &EcuConfig) -> Self {
        let routes = match &config.kind {
            NodeKind::Gateway { routes } => routes.clone(),
            NodeKind::Ecu => Vec::new(),
        };
        GatewayEcu {
            own: PeriodicEcu::new(config),
            routes,
            next_timer: FORWARD_TIMER_BASE,
            delayed: HashMap::new(),
        }
    }
}

impl Ecu for GatewayEcu {
    fn on_start(&mut self, ctx: &mut EcuCtx) {
        self.own.on_start(ctx);
    }

    fn on_timer(&mut self, timer: u32, ctx: &mut EcuCtx) {
        if timer < FORWARD_TIMER_BASE {
            self.own.on_timer(timer, ctx);
        } else if let Some((meta, bus, frame)) = self.delayed.remove(&timer) {
            ctx.forward_with(meta, bus, frame);
        }
    }

    fn on_frame(&mut self, bus: BusId, frame: &CanFrame, ctx: &mut EcuCtx) {
        let Some(meta) = ctx.incoming() else {
            return;
        };
        for route in &self.routes {
            if route.from_bus != bus || !route.filter.matches(frame) {
                continue;
            }
            let mut out = *frame;
            if let Some(id) = route.remap_id {
                out.id = if out.extended {
                    id & 0x1FFF_FFFF
                } else {
                    id & 0x7FF
                };
            }
            for &to in &route.to_buses {
                if route.delay_us == 0 {
                    ctx.forward_on(to, out);
                } else {
                    let timer = self.next_timer;
                    self.next_timer = self.next_timer.wrapping_add(1).max(FORWARD_TIMER_BASE);
                    self.delayed.insert(timer, (meta, to, out));
                    ctx.set_timer(timer, route.delay_us as u64 * 1_000);
                }
            }
        }
    }
}
