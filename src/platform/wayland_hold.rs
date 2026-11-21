//! Both edges check the compositor's stable window ID in the dispatching Lua
//! command. Resolving "active" again during cleanup could release into another app.
use super::{Duration, Wayland, Write, hypr_keys, quote};
use crate::{
    json,
    key_hold::{self, Result, Target},
    observation::Cancellation,
};

const IPC_BUDGET: Duration = Duration::from_millis(500);

struct WindowKey<'a> {
    backend: &'a Wayland,
    guard: String,
    args: String,
}

impl Wayland {
    pub(crate) fn hold(
        &self,
        chord: &str,
        window: &str,
        ms: u64,
        cancel: &Cancellation,
    ) -> Result<()> {
        let (key, mods) = hypr_keys::chord(chord)?;
        hypr_keys::selector(window)?;
        self.quiescent_keyboard()?;
        let active = self.request("j/activewindow")?;
        let (address, stable) = json::with_value(&active, |w| {
            let field = |name| {
                w.get(name)
                    .and_then(|v| v.string())
                    .map(|v| v.chars().collect::<String>())
            };
            Ok((field("address"), field("stableId")))
        })?;
        let address = address.ok_or("stale_target: no focused Hyprland window")?;
        let selector = hypr_keys::selector(&address)?;
        if window != "active" && !window.eq_ignore_ascii_case(&address) {
            return Err("stale_target: requested window is not focused".into());
        }
        let stable = stable
            .filter(|id| {
                !id.is_empty() && id.len() <= 32 && id.bytes().all(|b| b.is_ascii_hexdigit())
            })
            .ok_or("unsupported_state: compositor does not expose a stable window ID")?;
        let guard = format!(
            "local w=hl.get_active_window() if not w or w.address~={} or w.stable_id~={} or not w.visible or not w.accepts_input then return -1 end ",
            quote(&address),
            quote(&stable)
        );
        let args = format!(
            "mods={},key={},window={}",
            quote(&mods.join(" ")),
            quote(key),
            quote(&selector)
        );
        key_hold::run(
            &mut WindowKey {
                backend: self,
                guard,
                args,
            },
            ms,
            cancel,
        )
    }
}

impl WindowKey<'_> {
    fn command(
        &self,
        phase: Option<&str>,
        permit: Option<(&Cancellation, &mut bool)>,
    ) -> Result<()> {
        let action = match phase {
            Some(state) => format!(
                "local r=hl.dispatch(hl.dsp.send_key_state({{{},state={}}})) return r.ok and 1 or 0",
                self.args,
                quote(state)
            ),
            None => "return 1".into(),
        };
        let command = format!(
            "/repl return (function() {}if type(hl.dsp.send_key_state)~=\"function\" then return -2 end {} end)()",
            self.guard, action
        );
        let mut stream = self.backend.ipc_stream(IPC_BUDGET)?;
        let mut write = || {
            stream
                .write_all(command.as_bytes())
                .map_err(|e| e.to_string())
        };
        match permit {
            Some((token, attempted)) => token.dispatch(|| {
                *attempted = true;
                write()
            })?,
            None => write()?,
        }
        let reply = Wayland::read_reply(stream, IPC_BUDGET)?;
        match reply.trim() {
            "1" => Ok(()),
            "-1" => {
                Err("stale_target: original Hyprland window is no longer focused or valid".into())
            }
            "-2" => Err("unsupported_state: compositor lacks targeted key-state dispatch".into()),
            other => Err(format!(
                "Hyprland key hold could not confirm {:?}: {other}",
                phase.unwrap_or("target")
            )),
        }
    }
}

impl Target for WindowKey<'_> {
    fn verify(&mut self) -> Result<()> {
        self.command(None, None)
    }
    fn press(&mut self, cancel: &Cancellation, attempted: &mut bool) -> Result<()> {
        self.command(Some("down"), Some((cancel, attempted)))
    }
    fn release(&mut self) -> Result<()> {
        self.command(Some("up"), None)
    }
}
