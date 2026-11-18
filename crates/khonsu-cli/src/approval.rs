//! Who may approve a plugin from the command line.
//!
//! Approving a plugin lets it run as the person, unsandboxed, with their
//! secrets. A person does that, not an agent: when the environment shows an
//! agent running the command (see [`khonsu_dash::presence::detected_agent`]),
//! `plugin approve`, `plugin install --yes` and `plugin update --yes` approve
//! nothing and exit 4 (`needs_confirmation`). The person approves in the
//! window, where the plugin's permissions are shown beside the button.
//!
//! This stops an agent that approves because it was told it may, or because
//! a README says `--yes`; it does not stop one set on getting around it, which
//! can clear its environment. See `docs/plugin-security.md`.

use crate::CliError;
use serde_json::{json, Value};

/// Exit status for something only a person may do.
pub(crate) const NEEDS_CONFIRMATION: u8 = 4;

/// The agent that may not approve here, if this command is run by one.
pub(crate) fn approving_agent() -> Option<String> {
    khonsu_dash::presence::detected_agent()
}

/// The refusal for `agent` approving `plugins`, carrying `detail` (what was
/// done without approving, such as an install).
pub(crate) fn refusal(agent: &str, plugins: &[&str], detail: Value) -> CliError {
    let named = plugins.join(", ");
    CliError {
        kind: "needs_confirmation".into(),
        message: format!(
            "{agent} may not approve a plugin: approving lets it run as the person, with their secrets. \
             Ask the person to approve {named} in the KhonsuGraph window (Add source, then the plugin, then Approve), \
             or to run `khonsu plugin approve` themselves"
        ),
        detail: Box::new(json!({ "plugins": plugins, "agent": agent, "done": detail })),
        code: NEEDS_CONFIRMATION,
    }
}
