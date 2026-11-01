//! `/flow test` in the panel: what firing a flow would do now, in the pane until the next key.

use super::{App, Pane};
use jiff::Timestamp;
use rl_proto::flow::{FlowTest, FlowTestParams};

impl App {
    /// `/flow test <name>`: what firing it would do now, in the pane until the next key. Nothing
    /// starts.
    pub(super) async fn test_flow(&mut self, name: String) {
        let asked = FlowTestParams {
            name: name.clone(),
            event: None,
        };
        match self.rpc.call::<FlowTest>(asked).await {
            Ok(t) => {
                self.tested = Some(crate::flow::tested::lines(&t, Timestamp::now()));
                self.pane = Pane::Facts;
                self.cover_proposals();
                self.show_pane = true;
                self.say(format!("{name}: what would fire · started nothing"));
            }
            Err(e) => self.say(format!("test {name}: {e:#}")),
        }
    }
}
