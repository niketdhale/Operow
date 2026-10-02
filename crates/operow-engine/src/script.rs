use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use operow_core::{BusId, CanFrame};
use rhai::{AST, CallFnOptions, Dynamic, Engine, EvalAltResult, Map, Scope};

use crate::ecu::{Ecu, EcuCommand, EcuCtx};

/// Script timers are mapped onto internal timer ids in this range, which is
/// disjoint from the inner ECU's cyclic/deferred (< 0x4000_0000 .. 0x7000_0000)
/// and gateway (>= 0x8000_0000) ids.
const SCRIPT_TIMER_BASE: u32 = 0x7000_0000;
const SCRIPT_TIMER_END: u32 = 0x8000_0000;
const MAX_OPERATIONS: u64 = 1_000_000;

/// Actions requested by a script during one call; drained into the
/// [`EcuCtx`] afterwards.
#[derive(Default)]
struct Shared {
    now_ns: u64,
    sends: Vec<(Option<BusId>, CanFrame)>,
    timers: Vec<(i64, i64)>,
    commands: Vec<EcuCommand>,
    logs: Vec<String>,
}

type SharedRef = Arc<Mutex<Shared>>;

fn lock(shared: &SharedRef) -> std::sync::MutexGuard<'_, Shared> {
    shared.lock().unwrap_or_else(|e| e.into_inner())
}

fn rt_err(msg: String) -> Box<EvalAltResult> {
    msg.into()
}

fn get_int(map: &Map, key: &str) -> Option<i64> {
    map.get(key).and_then(|v| v.as_int().ok())
}

fn get_bool(map: &Map, key: &str) -> Option<bool> {
    map.get(key).and_then(|v| v.as_bool().ok())
}

/// Build a frame (and optional bus) from a script message map.
fn parse_message(map: &Map) -> Result<(Option<BusId>, CanFrame), String> {
    let id = get_int(map, "id").ok_or("message needs an integer `id`")?;
    let id = u32::try_from(id).map_err(|_| format!("invalid id {id}"))?;
    let extended = get_bool(map, "extended").unwrap_or(false);
    let fd = get_bool(map, "fd").unwrap_or(false);
    let brs = get_bool(map, "brs").unwrap_or(false);
    let mut data: Vec<u8> = match map.get("data") {
        Some(v) => v
            .clone()
            .into_array()
            .map_err(|_| "`data` must be an array")?
            .iter()
            .map(|b| {
                b.as_int()
                    .map(|n| n as u8)
                    .map_err(|_| "`data` must contain integers".to_string())
            })
            .collect::<Result<_, _>>()?,
        None => Vec::new(),
    };
    if let Some(dlc) = get_int(map, "dlc") {
        data.resize(dlc.clamp(0, 64) as usize, 0);
    }
    let frame = if fd {
        CanFrame::new_fd(id, extended, brs, &data)
    } else {
        CanFrame::new(id, extended, &data)
    }
    .map_err(|e| e.to_string())?;
    let bus = get_int(map, "bus").map(|b| BusId(b as u32));
    Ok((bus, frame))
}

fn frame_to_map(bus: BusId, frame: &CanFrame, now_ns: u64) -> Map {
    let mut m = Map::new();
    m.insert("id".into(), Dynamic::from(frame.id as i64));
    m.insert("extended".into(), Dynamic::from(frame.extended));
    m.insert("fd".into(), Dynamic::from(frame.fd));
    m.insert("dlc".into(), Dynamic::from(frame.dlc as i64));
    let data: rhai::Array = frame
        .payload()
        .iter()
        .map(|&b| Dynamic::from(b as i64))
        .collect();
    m.insert("data".into(), Dynamic::from(data));
    m.insert("bus".into(), Dynamic::from(bus.0 as i64));
    m.insert("time_ns".into(), Dynamic::from(now_ns as i64));
    m
}

/// Build the Rhai engine with the script API registered against `shared`.
fn build_engine(shared: &SharedRef) -> Engine {
    let mut engine = Engine::new();
    engine.set_max_operations(MAX_OPERATIONS);
    let s = shared.clone();
    engine.on_print(move |t| lock(&s).logs.push(t.to_string()));
    let s = shared.clone();
    engine.on_debug(move |t, _, _| lock(&s).logs.push(t.to_string()));
    let s = shared.clone();
    engine.register_fn(
        "output",
        move |msg: Map| -> Result<(), Box<EvalAltResult>> {
            let out = parse_message(&msg).map_err(rt_err)?;
            lock(&s).sends.push(out);
            Ok(())
        },
    );
    let s = shared.clone();
    engine.register_fn("set_timer", move |id: i64, ms: i64| {
        lock(&s).timers.push((id, ms.max(0)));
    });
    let s = shared.clone();
    engine.register_fn("now_ns", move || lock(&s).now_ns as i64);
    let s = shared.clone();
    engine.register_fn("now_ms", move || (lock(&s).now_ns / 1_000_000) as i64);
    let s = shared.clone();
    engine.register_fn("trigger", move |msg: i64| {
        if msg >= 0 {
            lock(&s)
                .commands
                .push(EcuCommand::Trigger { msg: msg as usize });
        }
    });
    let s = shared.clone();
    engine.register_fn(
        "set_payload",
        move |msg: i64, data: rhai::Array| -> Result<(), Box<EvalAltResult>> {
            let data = data
                .iter()
                .map(|b| b.as_int().map(|n| n as u8))
                .collect::<Result<Vec<u8>, _>>()
                .map_err(|_| rt_err("set_payload data must contain integers".into()))?;
            if msg >= 0 {
                lock(&s).commands.push(EcuCommand::SetPayload {
                    msg: msg as usize,
                    data,
                });
            }
            Ok(())
        },
    );
    engine
}

/// Compile `src` without running it, using the same engine setup as
/// [`ScriptEcu`] so registered functions resolve. The error message includes
/// the line/position.
pub fn check_script(src: &str) -> Result<(), String> {
    let shared: SharedRef = Arc::new(Mutex::new(Shared::default()));
    build_engine(&shared)
        .compile(src)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Wraps a node's normal ECU and additionally runs CAPL-like Rhai handlers
/// (`on_start()`, `on_timer(id)`, `on_message(msg)`). Built-in transmission
/// and routing of the inner ECU keep working.
///
/// Functions in Rhai cannot see top-level variables, so persistent script
/// state lives in `this` (an object map shared by all handlers), e.g.
/// `this.count += 1`. Top-level statements run once at start.
pub struct ScriptEcu {
    inner: Box<dyn Ecu>,
    name: String,
    engine: Engine,
    ast: AST,
    scope: Scope<'static>,
    state: Dynamic,
    shared: SharedRef,
    has_start: bool,
    has_timer: bool,
    has_message: bool,
    timers: HashMap<u32, i64>,
    next_token: u32,
    logs: Vec<String>,
}

impl ScriptEcu {
    /// Compile `source`; returns the compile error message on failure.
    pub fn new(inner: Box<dyn Ecu>, name: &str, source: &str) -> Result<Self, String> {
        let shared: SharedRef = Arc::new(Mutex::new(Shared::default()));
        let engine = build_engine(&shared);

        let ast = engine.compile(source).map_err(|e| e.to_string())?;
        let has = |fname: &str, params: usize| {
            ast.iter_functions()
                .any(|f| f.name == fname && f.params.len() == params)
        };
        let (has_start, has_timer, has_message) =
            (has("on_start", 0), has("on_timer", 1), has("on_message", 1));
        Ok(ScriptEcu {
            inner,
            name: name.to_string(),
            engine,
            ast,
            scope: Scope::new(),
            state: Dynamic::from(Map::new()),
            shared,
            has_start,
            has_timer,
            has_message,
            timers: HashMap::new(),
            next_token: SCRIPT_TIMER_BASE,
            logs: Vec::new(),
        })
    }

    fn log(&mut self, now_ns: u64, text: &str) {
        self.logs.push(format!(
            "[{} {:.3}ms] {}",
            self.name,
            now_ns as f64 / 1e6,
            text
        ));
    }

    /// Run `f` against the engine, then move collected prints/actions into
    /// `ctx`. Runtime errors are logged.
    fn call(
        &mut self,
        ctx: &mut EcuCtx,
        f: impl FnOnce(
            &Engine,
            &AST,
            &mut Scope<'static>,
            &mut Dynamic,
        ) -> Result<(), Box<EvalAltResult>>,
    ) {
        let now = ctx.now().0;
        lock(&self.shared).now_ns = now;
        let result = f(&self.engine, &self.ast, &mut self.scope, &mut self.state);
        let (sends, timers, commands, logs) = {
            let mut sh = lock(&self.shared);
            (
                std::mem::take(&mut sh.sends),
                std::mem::take(&mut sh.timers),
                std::mem::take(&mut sh.commands),
                std::mem::take(&mut sh.logs),
            )
        };
        for l in logs {
            self.log(now, &l);
        }
        if let Err(e) = result {
            self.log(now, &format!("script error: {e}"));
        }
        for (bus, frame) in sends {
            match bus {
                Some(b) => ctx.send_on(b, frame),
                None => ctx.send(frame),
            }
        }
        for (id, ms) in timers {
            let token = self.next_token;
            self.next_token = if token + 1 >= SCRIPT_TIMER_END {
                SCRIPT_TIMER_BASE
            } else {
                token + 1
            };
            self.timers.insert(token, id);
            ctx.set_timer(token, ms as u64 * 1_000_000);
        }
        for cmd in commands {
            self.inner.on_command(&cmd, ctx);
        }
    }

    fn call_handler(&mut self, ctx: &mut EcuCtx, fname: &'static str, arg: Dynamic) {
        self.call(ctx, |engine, ast, scope, state| {
            let opts = CallFnOptions::new()
                .eval_ast(false)
                .rewind_scope(false)
                .bind_this_ptr(state);
            engine
                .call_fn_with_options::<Dynamic>(opts, scope, ast, fname, (arg,))
                .map(|_| ())
        });
    }
}

impl Ecu for ScriptEcu {
    fn on_start(&mut self, ctx: &mut EcuCtx) {
        self.inner.on_start(ctx);
        self.call(ctx, |engine, ast, scope, _| {
            engine.run_ast_with_scope(scope, ast)
        });
        if self.has_start {
            self.call(ctx, |engine, ast, scope, state| {
                let opts = CallFnOptions::new()
                    .eval_ast(false)
                    .rewind_scope(false)
                    .bind_this_ptr(state);
                engine
                    .call_fn_with_options::<Dynamic>(opts, scope, ast, "on_start", ())
                    .map(|_| ())
            });
        }
    }

    fn on_timer(&mut self, timer: u32, ctx: &mut EcuCtx) {
        if (SCRIPT_TIMER_BASE..SCRIPT_TIMER_END).contains(&timer) {
            if let Some(id) = self.timers.remove(&timer)
                && self.has_timer
            {
                self.call_handler(ctx, "on_timer", Dynamic::from(id));
            }
        } else {
            self.inner.on_timer(timer, ctx);
        }
    }

    fn on_frame(&mut self, bus: BusId, frame: &CanFrame, ctx: &mut EcuCtx) {
        self.inner.on_frame(bus, frame, ctx);
        if self.has_message {
            let msg = frame_to_map(bus, frame, ctx.now().0);
            self.call_handler(ctx, "on_message", Dynamic::from(msg));
        }
    }

    fn on_command(&mut self, cmd: &EcuCommand, ctx: &mut EcuCtx) {
        self.inner.on_command(cmd, ctx);
    }

    fn drain_logs(&mut self) -> Vec<String> {
        std::mem::take(&mut self.logs)
    }
}

#[cfg(test)]
mod check_tests {
    use super::check_script;

    #[test]
    fn valid_script_compiles() {
        assert!(
            check_script("fn on_message(msg) { output(#{ id: 1 }); set_timer(1, 10); }").is_ok()
        );
        assert!(check_script("").is_ok());
    }

    #[test]
    fn syntax_error_reports_position() {
        let err = check_script("fn on_start() {\n    let x = ;\n}\n").unwrap_err();
        assert!(err.contains("line 2"), "{err}");
    }
}
