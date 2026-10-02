use operow_core::{BusId, CanFrame, EcuConfig, NodeId, Timestamp, TxMessage};

/// Bookkeeping that travels with a frame through the simulation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameMeta {
    /// Node driving the wire.
    pub sender: NodeId,
    /// ECU that first created the frame.
    pub origin: NodeId,
    /// Unique per originated frame; shared by all forwarded copies.
    pub uid: u64,
    /// Number of gateways crossed so far.
    pub hop: u8,
}

/// A frame queued by an ECU callback.
pub(crate) struct Outgoing {
    /// Target bus; `None` means every bus the node is attached to.
    pub(crate) bus: Option<BusId>,
    pub(crate) frame: CanFrame,
    /// Meta of the frame being forwarded; `None` for a newly originated one.
    pub(crate) forward_of: Option<FrameMeta>,
}

/// Context handed to an [`Ecu`] callback. Lets the ECU send frames (on every
/// bus it is attached to, or on one) and arm timers; actions are collected
/// and applied by the simulation after the callback returns.
pub struct EcuCtx {
    pub(crate) node: NodeId,
    pub(crate) now: Timestamp,
    pub(crate) incoming: Option<FrameMeta>,
    pub(crate) sends: Vec<Outgoing>,
    pub(crate) timers: Vec<(u32, u64)>,
}

impl EcuCtx {
    pub(crate) fn new(node: NodeId, now: Timestamp, incoming: Option<FrameMeta>) -> Self {
        EcuCtx {
            node,
            now,
            incoming,
            sends: Vec::new(),
            timers: Vec::new(),
        }
    }

    /// Meta of the frame being delivered to `on_frame`; `None` in other
    /// callbacks.
    pub fn incoming(&self) -> Option<FrameMeta> {
        self.incoming
    }

    /// The node this context belongs to.
    pub fn node(&self) -> NodeId {
        self.node
    }

    /// Current virtual simulation time.
    pub fn now(&self) -> Timestamp {
        self.now
    }

    /// Queue `frame` to be transmitted on every bus this node is attached to.
    pub fn send(&mut self, frame: CanFrame) {
        self.sends.push(Outgoing {
            bus: None,
            frame,
            forward_of: None,
        });
    }

    /// Queue `frame` to be transmitted on `bus` only. Dropped if this node
    /// is not attached to `bus`.
    pub fn send_on(&mut self, bus: BusId, frame: CanFrame) {
        self.sends.push(Outgoing {
            bus: Some(bus),
            frame,
            forward_of: None,
        });
    }

    /// Forward the frame currently being received (see [`EcuCtx::incoming`])
    /// onto `bus`, preserving its uid and origin and incrementing its hop.
    /// `frame` is the (possibly modified) frame to send. Does nothing
    /// outside `on_frame`.
    pub fn forward_on(&mut self, bus: BusId, frame: CanFrame) {
        if let Some(meta) = self.incoming {
            self.forward_with(meta, bus, frame);
        }
    }

    /// Like [`EcuCtx::forward_on`] but for a frame whose `meta` was saved
    /// earlier, e.g. to forward from a timer after a delay.
    pub fn forward_with(&mut self, meta: FrameMeta, bus: BusId, frame: CanFrame) {
        self.sends.push(Outgoing {
            bus: Some(bus),
            frame,
            forward_of: Some(meta),
        });
    }

    /// Arm a timer identified by `timer_id`, firing `on_timer` after
    /// `delay_ns` nanoseconds of virtual time.
    pub fn set_timer(&mut self, timer_id: u32, delay_ns: u64) {
        self.timers.push((timer_id, delay_ns));
    }
}

/// Behavior of a simulated ECU.
pub trait Ecu: Send {
    /// Called once at simulation start (virtual time 0).
    fn on_start(&mut self, _ctx: &mut EcuCtx) {}
    /// Called when a timer previously armed with `EcuCtx::set_timer` fires.
    fn on_timer(&mut self, _timer: u32, _ctx: &mut EcuCtx) {}
    /// Called when a frame sent by another node arrives on `bus`.
    fn on_frame(&mut self, _bus: BusId, _frame: &CanFrame, _ctx: &mut EcuCtx) {}
}

/// An [`Ecu`] that periodically transmits the messages defined in an
/// [`EcuConfig`]: each enabled message is first sent at t=0 and then re-sent
/// every `period_ms`.
pub struct PeriodicEcu {
    messages: Vec<TxMessage>,
}

impl PeriodicEcu {
    pub fn new(config: &EcuConfig) -> Self {
        PeriodicEcu {
            messages: config.tx.clone(),
        }
    }
}

impl Ecu for PeriodicEcu {
    fn on_start(&mut self, ctx: &mut EcuCtx) {
        for (i, msg) in self.messages.iter().enumerate() {
            if msg.enabled {
                ctx.set_timer(i as u32, 0);
            }
        }
    }

    fn on_timer(&mut self, timer: u32, ctx: &mut EcuCtx) {
        if let Some(msg) = self.messages.get(timer as usize)
            && msg.enabled
        {
            match msg.bus {
                Some(bus) => ctx.send_on(bus, msg.frame),
                None => ctx.send(msg.frame),
            }
            ctx.set_timer(timer, msg.period_ms as u64 * 1_000_000);
        }
    }
}
