//! Scripted UI smoke tests.
//!
//! Set `PDFEDIT_SCRIPT` to a `;`-separated list of steps and the window plays
//! them back, one every few frames:
//!
//! ```text
//! shot:/tmp/a.png   save a screenshot of the window
//! move:300,120      move the pointer (window points)
//! click:300,120     click
//! type:Hello        type text
//! key:Enter         press Enter, Escape or Backspace
//! undo              press Cmd/Ctrl+Z
//! quit              close the window
//! ```

use std::collections::VecDeque;
use std::path::PathBuf;

use eframe::egui::{
    self, Event, Key, Modifiers, PointerButton, Pos2, ScreenshotCallback, ViewportCommand,
};

const FRAMES_PER_STEP: u32 = 30;

enum Step {
    Shot(PathBuf),
    Move(Pos2),
    Click(Pos2),
    Type(String),
    Key(Key, Modifiers),
    Quit,
}

pub struct Script {
    steps: VecDeque<Step>,
    frame: u32,
    release: Option<Pos2>,
}

fn parse_pos(s: &str) -> Option<Pos2> {
    let (x, y) = s.split_once(',')?;
    Some(Pos2::new(x.trim().parse().ok()?, y.trim().parse().ok()?))
}

impl Script {
    /// Read the script from the environment, if one is set.
    pub fn from_env() -> Option<Self> {
        let text = std::env::var("PDFEDIT_SCRIPT").ok()?;
        let steps = text
            .split(';')
            .filter_map(|step| {
                let (name, arg) = step.trim().split_once(':').unwrap_or((step.trim(), ""));
                Some(match name {
                    "shot" => Step::Shot(PathBuf::from(arg)),
                    "move" => Step::Move(parse_pos(arg)?),
                    "click" => Step::Click(parse_pos(arg)?),
                    "type" => Step::Type(arg.to_string()),
                    "key" => Step::Key(Key::from_name(arg)?, Modifiers::NONE),
                    "undo" => Step::Key(Key::Z, Modifiers::COMMAND),
                    "quit" => Step::Quit,
                    _ => return None,
                })
            })
            .collect();
        Some(Self {
            steps,
            frame: 0,
            release: None,
        })
    }

    /// Inject this frame's scripted input.
    pub fn tick(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        ctx.request_repaint();
        if let Some(pos) = self.release.take() {
            raw.events.push(Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed: false,
                modifiers: Modifiers::NONE,
            });
            return;
        }
        self.frame += 1;
        if !self.frame.is_multiple_of(FRAMES_PER_STEP) {
            return;
        }
        match self.steps.pop_front() {
            Some(Step::Shot(path)) => {
                ctx.send_viewport_cmd(ViewportCommand::Screenshot(ScreenshotCallback::new(
                    move |image| {
                        let _ = image::save_buffer(
                            &path,
                            image.as_raw(),
                            image.width() as u32,
                            image.height() as u32,
                            image::ExtendedColorType::Rgba8,
                        );
                    },
                )));
            }
            Some(Step::Move(pos)) => raw.events.push(Event::PointerMoved(pos)),
            Some(Step::Click(pos)) => {
                raw.events.push(Event::PointerMoved(pos));
                raw.events.push(Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed: true,
                    modifiers: Modifiers::NONE,
                });
                self.release = Some(pos);
            }
            Some(Step::Type(text)) => raw.events.push(Event::Text(text)),
            Some(Step::Key(key, modifiers)) => {
                for pressed in [true, false] {
                    raw.events.push(Event::Key {
                        key,
                        physical_key: None,
                        pressed,
                        repeat: false,
                        modifiers,
                    });
                }
            }
            Some(Step::Quit) | None => ctx.send_viewport_cmd(ViewportCommand::Close),
        }
    }
}
