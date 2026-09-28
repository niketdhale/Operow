use operow_core::{BusId, CanFrame, EcuConfig, NodeId, Timestamp, TxMessage};

/// Context handed to an [`Ecu`] callback. Lets the ECU send frames (on every
/// bus it is attached to) and arm timers; actions are collected and applied
/// by the simulation after the callback returns.
pub struct EcuCtx {
    pub(crate) node: NodeId,
    pub(crate) now: Timestamp,
    pub(crate) buses: Vec<BusId>,
    pub(crate) sends: Vec<CanFrame>,
    pub(crate) timers: Vec<(u32, u64)>,
}

impl EcuCtx {
    pub(crate) fn new(node: NodeId, now: Timestamp, buses: Vec<BusId>) -> Self {
        EcuCtx {
            node,
            now,
            buses,
            sends: Vec::new(),
            timers: Vec::new(),
        }
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
        self.sends.push(frame);
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
            ctx.send(msg.frame);
            ctx.set_timer(timer, msg.period_ms as u64 * 1_000_000);
        }
    }
}
