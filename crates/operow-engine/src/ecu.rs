use operow_core::{BusId, CanFrame, EcuConfig, NodeId, SendType, Timestamp, TxMessage};

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

/// A command delivered to an ECU from outside the simulation (e.g. the UI).
/// `msg` indexes the node's `tx` list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EcuCommand {
    /// Fire an event send of the message.
    Trigger { msg: usize },
    /// Replace the payload bytes, keeping the frame id and flags.
    SetPayload { msg: usize, data: Vec<u8> },
    /// Activate or deactivate a `CyclicIfActive` message.
    SetActive { msg: usize, active: bool },
}

/// Behavior of a simulated ECU.
pub trait Ecu: Send {
    /// Called once at simulation start (virtual time 0).
    fn on_start(&mut self, _ctx: &mut EcuCtx) {}
    /// Called when a timer previously armed with `EcuCtx::set_timer` fires.
    fn on_timer(&mut self, _timer: u32, _ctx: &mut EcuCtx) {}
    /// Called when a frame sent by another node arrives on `bus`.
    fn on_frame(&mut self, _bus: BusId, _frame: &CanFrame, _ctx: &mut EcuCtx) {}
    /// Called when an [`EcuCommand`] is delivered to this node.
    fn on_command(&mut self, _cmd: &EcuCommand, _ctx: &mut EcuCtx) {}
    /// Run the node script's handler `name` (`on_diag`, `on_security_key`)
    /// with `arg` as a byte array. `None` when no script handles it;
    /// `Some(bytes)` is the array the handler returned.
    fn script_hook(&mut self, _name: &str, _arg: &[u8], _ctx: &mut EcuCtx) -> Option<Vec<u8>> {
        None
    }
    /// Take any log lines produced since the last call.
    fn drain_logs(&mut self) -> Vec<String> {
        Vec::new()
    }
}

/// Timer ids at or above this value are deferred on-change sends
/// (`DEFERRED_TIMER_BASE + msg`); below it are cyclic timers.
pub(crate) const DEFERRED_TIMER_BASE: u32 = 0x4000_0000;
const MSG_BITS: u32 = 20;
const MSG_MASK: u32 = (1 << MSG_BITS) - 1;
const GEN_MASK: u32 = 0x3FF;

/// Cyclic timer id: generation in bits 20..30, message index below.
fn cyclic_timer(msg: usize, generation: u32) -> u32 {
    ((generation & GEN_MASK) << MSG_BITS) | (msg as u32 & MSG_MASK)
}

struct MsgState {
    msg: TxMessage,
    active: bool,
    last_send: Option<u64>,
    /// A deferred on-change send is waiting for its timer.
    pending: bool,
    /// Bumped whenever the cycle is (re)started or stopped; cyclic timers
    /// from an older generation are ignored.
    generation: u32,
}

/// An [`Ecu`] that transmits the messages defined in an [`EcuConfig`]
/// according to their [`SendType`]; see that type for the semantics.
/// Cyclic messages are first sent at t=0 and then every `period_ms`.
pub struct PeriodicEcu {
    messages: Vec<MsgState>,
}

impl PeriodicEcu {
    pub fn new(config: &EcuConfig) -> Self {
        PeriodicEcu {
            messages: config
                .tx
                .iter()
                .map(|msg| MsgState {
                    msg: msg.clone(),
                    active: false,
                    last_send: None,
                    pending: false,
                    generation: 0,
                })
                .collect(),
        }
    }
}

impl MsgState {
    fn cyclic_running(&self) -> bool {
        self.msg.enabled
            && match self.msg.send_type {
                SendType::Cyclic | SendType::CyclicAndEvent => true,
                SendType::CyclicIfActive => self.active,
                SendType::Event | SendType::OnChange { .. } => false,
            }
    }

    fn send(&mut self, ctx: &mut EcuCtx) {
        match self.msg.bus {
            Some(bus) => ctx.send_on(bus, self.msg.frame),
            None => ctx.send(self.msg.frame),
        }
        self.last_send = Some(ctx.now().0);
    }

    /// Send now, or for `OnChange` defer to `last_send + min_gap`.
    fn send_on_change(&mut self, msg: usize, min_gap_ms: u32, ctx: &mut EcuCtx) {
        let now = ctx.now().0;
        let due = self
            .last_send
            .map_or(now, |t| t + min_gap_ms as u64 * 1_000_000);
        if due <= now {
            self.pending = false;
            self.send(ctx);
        } else if !self.pending {
            self.pending = true;
            ctx.set_timer(DEFERRED_TIMER_BASE + msg as u32, due - now);
        }
    }
}

impl Ecu for PeriodicEcu {
    fn on_start(&mut self, ctx: &mut EcuCtx) {
        for (i, st) in self.messages.iter().enumerate() {
            if st.cyclic_running() {
                ctx.set_timer(cyclic_timer(i, st.generation), 0);
            }
        }
    }

    fn on_timer(&mut self, timer: u32, ctx: &mut EcuCtx) {
        if timer >= DEFERRED_TIMER_BASE {
            if let Some(st) = self
                .messages
                .get_mut((timer - DEFERRED_TIMER_BASE) as usize)
                && st.pending
            {
                st.pending = false;
                if st.msg.enabled {
                    st.send(ctx);
                }
            }
            return;
        }
        let idx = (timer & MSG_MASK) as usize;
        let generation = timer >> MSG_BITS;
        if let Some(st) = self.messages.get_mut(idx)
            && st.generation & GEN_MASK == generation
            && st.cyclic_running()
        {
            st.send(ctx);
            ctx.set_timer(timer, st.msg.period_ms as u64 * 1_000_000);
        }
    }

    fn on_command(&mut self, cmd: &EcuCommand, ctx: &mut EcuCtx) {
        match cmd {
            EcuCommand::Trigger { msg } => {
                let Some(st) = self.messages.get_mut(*msg).filter(|s| s.msg.enabled) else {
                    return;
                };
                match st.msg.send_type {
                    SendType::Event | SendType::CyclicAndEvent => st.send(ctx),
                    SendType::OnChange { min_gap_ms } => st.send_on_change(*msg, min_gap_ms, ctx),
                    SendType::Cyclic | SendType::CyclicIfActive => {}
                }
            }
            EcuCommand::SetPayload { msg, data } => {
                let Some(st) = self.messages.get_mut(*msg) else {
                    return;
                };
                let old = st.msg.frame;
                let len = (st.msg.frame.dlc as usize).min(data.len());
                st.msg.frame.data[..len].copy_from_slice(&data[..len]);
                if !st.msg.enabled {
                    return;
                }
                match st.msg.send_type {
                    SendType::CyclicAndEvent => st.send(ctx),
                    SendType::OnChange { min_gap_ms } if st.msg.frame != old => {
                        st.send_on_change(*msg, min_gap_ms, ctx)
                    }
                    _ => {}
                }
            }
            EcuCommand::SetActive { msg, active } => {
                let Some(st) = self.messages.get_mut(*msg) else {
                    return;
                };
                if st.active == *active {
                    return;
                }
                st.active = *active;
                if st.msg.send_type == SendType::CyclicIfActive {
                    // Invalidate any timer chain, then start a fresh one.
                    st.generation = st.generation.wrapping_add(1);
                    if st.cyclic_running() {
                        ctx.set_timer(cyclic_timer(*msg, st.generation), 0);
                    }
                }
            }
        }
    }
}
