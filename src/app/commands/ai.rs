//! AI commands: AISETTINGS (provider settings) and the AI-assisted tools
//! built on the shared provider layer in `crate::ai`.

use crate::app::{Message, OpenCADStudio};
use iced::Task;

impl OpenCADStudio {
    pub(super) fn dispatch_ai(&mut self, cmd: &str, _i: usize) -> Option<Task<Message>> {
        let (verb, _rest) = cmd.split_once(char::is_whitespace).unwrap_or((cmd, ""));
        match verb
            .trim_start_matches(['_', '.'])
            .to_ascii_uppercase()
            .as_str()
        {
            "AISETTINGS" | "AIOPTIONS" => {
                self.open_ai_settings();
                Some(Task::none())
            }
            "AICMD" | "AI" => {
                self.open_ai_assistant(crate::ui::window::ai_assistant::Mode::Command);
                Some(Task::none())
            }
            "AICHAT" | "AIASK" => {
                self.open_ai_assistant(crate::ui::window::ai_assistant::Mode::Chat);
                Some(Task::none())
            }
            "AIIMAGE" | "IMG2CAD" => {
                self.open_ai_image(crate::ai::image_to_model::Task::MultiView);
                Some(Task::none())
            }
            "AISKETCH" | "SKETCH2CAD" => {
                self.open_ai_image(crate::ai::image_to_model::Task::Sketch);
                Some(Task::none())
            }
            "AICHECK" | "DWGCHECK" => {
                self.open_ai_check();
                Some(Task::none())
            }
            "AICONSTRAIN" => {
                self.open_ai_constrain();
                Some(Task::none())
            }
            "AIPART" | "TEXT2PART" => {
                self.open_ai_part();
                Some(Task::none())
            }
            "AIPLAN" | "FLOORPLAN" => {
                self.open_ai_plan();
                Some(Task::none())
            }
            _ => None,
        }
    }
}
