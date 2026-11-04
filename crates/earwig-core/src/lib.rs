//! earwig's core: everything except pixels.
//!
//! The daemon is a [`controller`] that owns the dictation state machine
//! ([`session`]), the microphone ([`audio`]), a loaded speech model
//! ([`engine`]), and an inserter ([`insert`]). Hotkeys arrive as
//! [`events::Command`]s from the control socket ([`ipc`]) or the portal
//! ([`hotkey`]); everything that happens leaves as [`events::Event`]s, which
//! the GPUI front end, `earwig events`, and status bars consume.

pub mod config;
pub mod desktop;
pub mod engine;
pub mod events;
pub mod hotkey;
pub mod insert;
pub mod models;

/// Sample rate of all audio handed to an engine: 16 kHz mono `f32`.
pub const SAMPLE_RATE: u32 = 16_000;

// Modules owned by the pipeline slice, with the surface other parts use:
//
// paths     XDG locations
//   fn user_config() -> PathBuf          $XDG_CONFIG_HOME/earwig/config.toml
//   fn system_configs() -> Vec<PathBuf>  earwig/config.toml in $XDG_CONFIG_DIRS
//   fn data_dir() -> PathBuf             $XDG_DATA_HOME/earwig
//   fn models_dir() -> PathBuf           data_dir()/models
//   fn history_file() -> PathBuf         data_dir()/history.jsonl
//   fn state_dir() -> PathBuf            $XDG_STATE_HOME/earwig (portal tokens)
//   fn socket() -> PathBuf               $XDG_RUNTIME_DIR/earwig.sock
//
// session   pure tap-or-hold state machine; time is passed in
//
// audio     cpal capture, resampled to SAMPLE_RATE, pre-roll ring buffer
//   fn input_devices() -> anyhow::Result<Vec<String>>
//
// history
//   pub struct HistoryEntry { at_ms: u64, text: String, duration_ms: u64,
//                             model: String, method: Option<InsertMethod> }
//   impl History { fn open(path, limit) -> Result<Self>; fn entries(&self) -> Vec<HistoryEntry>;
//                  fn clear(&self) -> Result<()> }   // &self, internally locked
//
// hooks     run HooksConfig commands
//
// controller
//   pub struct Parts { config: ConfigStore, models: Arc<ModelManager>,
//                      history: Arc<History>, engine: Arc<dyn EngineFactory>,
//                      inserter: Box<dyn Inserter>, caps: DesktopCaps,
//                      hotkey_sources: Vec<String> }
//   pub fn spawn(parts: Parts) -> anyhow::Result<Handle>
//   #[derive(Clone)] pub struct Handle
//   impl Handle {
//       fn send(&self, cmd: Command)
//       fn commands(&self) -> flume::Sender<Command>   // for hotkey sources / ipc
//       fn subscribe(&self) -> flume::Receiver<Event>  // starts with Event::Status
//       fn status(&self) -> Status
//       fn config(&self) -> Arc<RwLock<ConfigStore>>   // settings window edits, then ReloadConfig
//       fn models(&self) -> Arc<ModelManager>
//       fn history(&self) -> Arc<History>
//       fn engine(&self) -> Arc<dyn EngineFactory>
//   }
//
// ipc       control socket: one JSON Command per line in, Events out
//   fn serve(handle: controller::Handle) -> anyhow::Result<()>   // spawns a thread
//   fn request(cmd: &Command) -> anyhow::Result<Option<Event>>   // client side
//   fn subscribe() -> anyhow::Result<impl Iterator<Item = anyhow::Result<Event>>>
