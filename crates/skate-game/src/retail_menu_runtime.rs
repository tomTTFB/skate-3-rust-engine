//! A running retail menu movie (doc 31, milestone 5a): one linked movie from `RetailMenuMovies`
//! on its own VM, with the HUD's lifecycle (init actions, initialize, advance, drain the
//! action queue). No Bevy, so it runs in tests against the exported set.
//!
//! The host answers the generic natives the HUD already implements. Every other host call the
//! movie makes is answered `undefined` and recorded with its arguments' types, so the menu keeps
//! drawing while the host surface is still unknown. Milestone 5b reads that record and implements
//! the calls (CoreMenu, GameSettings_*) against the menu registry.
use crate::{
    apt_movie::{Movie, Pending},
    apt_vm::{Host, ObjectKind, Value, Vm},
};
use std::collections::BTreeMap;

/// Action blocks per drain before the frame is abandoned (the HUD uses the same bound).
const ACTION_LIMIT: usize = 4096;
/// Native objects created before init actions run, as in `hud_runtime::Runtime::load`. A method
/// call on an undefined global never reaches the host, so every object the menus call into must
/// exist for its calls to be answered or recorded: `ScreenManager` (screens register with
/// `ScreenManager.OnLoaded`) and `coremenu` (the crossbar's data: GetIsDemo, GetMaxSubOptions,
/// GetProgressionData, ...).
const NATIVES: [&str; 5] = ["MovieClip", "Math", "FELanguage", "ScreenManager", "coremenu"];

/// What `coremenu` answers: the crossbar of the current mode, resolved by the menu registry
/// (visible tabs and rows, mod edits and greying rules applied). Labels are language-table ids,
/// which the movie's text fields localize. Built by the game when the menu opens.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CrossBar {
    /// Mode title id (`ID_CROSSBAR_..._MODE_TITLE`).
    pub title: String,
    pub tabs: Vec<CrossBarTab>,
    /// Selected tab and row.
    pub menu: usize,
    pub option: usize,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CrossBarTab {
    pub label: String,
    /// Frame label of the retail icon sprite (`map`, `settings`, ...); "" for a mod image,
    /// which the menus cannot draw yet.
    pub icon: String,
    pub rows: Vec<CrossBarRow>,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CrossBarRow {
    pub label: String,
    /// Frame label of the retail item icon sprite (`freeskate`, `settings`, ...); "" for a mod
    /// image. NOT RETAIL YET: the disc's `flagpark` and `loadpark` item icons have no label in
    /// that sprite; retail Flash ignores an unknown label, our VM reports it (park modes only).
    pub icon: String,
    pub enabled: bool,
}

/// A registered screen. core_menu's RegisterScreen pushes `this` (the controller, the class
/// instance with InitializeMenu / UpdateInput) and then `core_menu.mScreen` (the display clip),
/// so the call is `OnLoaded(clip, controller)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Screen {
    pub clip: usize,
    pub controller: usize,
}

pub struct MenuHost {
    pub movie: Movie,
    random: u32,
    /// Screens registered through `ScreenManager.OnLoaded(clip, controller)`, in order.
    pub screens: Vec<Screen>,
    pub crossbar: CrossBar,
    /// "Native.method(arg types)" -> number of calls, for every call the host does not implement.
    pub unhandled: BTreeMap<String, usize>,
    /// Script `trace` output, oldest first (bounded).
    pub traces: Vec<String>,
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Undefined => "undefined",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::Text(_) => "string",
        Value::Object(_) => "object",
    }
}

/// A script index argument as a position (non-negative whole numbers only).
fn index(value: Option<&Value>) -> Option<usize> {
    let n = value?.number();
    (n.is_finite() && n >= 0.0 && n.fract() == 0.0).then_some(n as usize)
}

impl MenuHost {
    fn tab(&self, value: Option<&Value>) -> Option<&CrossBarTab> {
        self.crossbar.tabs.get(index(value)?)
    }
    /// The row named by `(tab, row)` arguments.
    fn row(&self, args: &[Value]) -> Option<&CrossBarRow> {
        self.tab(args.first())?.rows.get(index(args.get(1))?)
    }
}

impl Host for MenuHost {
    fn property_changed(&mut self, vm: &mut Vm, object: usize, key: &str) -> Result<(), String> {
        if key == "text" || key == "autoSize" {
            self.movie.text_changed(vm, object)?;
        }
        Ok(())
    }
    fn trace(&mut self, message: &str) {
        if self.traces.len() < 256 {
            self.traces.push(message.to_owned());
        }
    }
    fn call(
        &mut self,
        vm: &mut Vm,
        object: usize,
        method: &str,
        args: Vec<Value>,
    ) -> Result<Value, String> {
        if self.movie.method(vm, object, method, &args)? {
            return Ok(Value::Undefined);
        }
        let native = match vm.objects.get(object).map(|o| &o.kind) {
            Some(ObjectKind::Native(name)) => name.as_str(),
            _ => "global",
        };
        match (native, method) {
            ("Math", "floor") => Ok(Value::Number(
                args.first().ok_or("Math.floor argument")?.number().floor(),
            )),
            ("Math", "random") => {
                // Presentation-only random source, as in the HUD.
                self.random ^= self.random << 13;
                self.random ^= self.random >> 17;
                self.random ^= self.random << 5;
                Ok(Value::Number(self.random as f64 / 4294967296.0))
            }
            ("global", "ASSetPropFlags") => Ok(Value::Undefined),
            ("FELanguage", "GetLanguage") => Ok(Value::Text("english".into())),
            // A screen's RegisterScreen passes itself here; the screen manager shows it later.
            ("ScreenManager", "OnLoaded") => {
                let (Some(Value::Object(clip)), Some(Value::Object(controller))) =
                    (args.first(), args.get(1))
                else {
                    return Err(format!("ScreenManager.OnLoaded expects (clip, controller): {args:?}"));
                };
                let screen = Screen {
                    clip: *clip,
                    controller: *controller,
                };
                if !self.screens.contains(&screen) {
                    self.screens.push(screen);
                }
                Ok(Value::Undefined)
            }
            // The crossbar's data, from the registry snapshot. Out-of-range indices read as an
            // empty tab, so a script asking past the end gets 0 / "" / false, never undefined
            // (undefined turns the layout arithmetic into NaN).
            ("coremenu", "GetNumMenus") => Ok(Value::Number(self.crossbar.tabs.len() as f64)),
            ("coremenu", "GetCurrentMenuIndex") => Ok(Value::Number(self.crossbar.menu as f64)),
            ("coremenu", "GetCurrentOptionIndex") => {
                Ok(Value::Number(self.crossbar.option as f64))
            }
            ("coremenu", "GetCurrentModeTitle") => Ok(Value::Text(self.crossbar.title.clone())),
            ("coremenu", "GetMenuName") => Ok(Value::Text(
                self.tab(args.first()).map_or_else(String::new, |t| t.label.clone()),
            )),
            // The movie plays the icon sprite at this label (gotoAndPlay); a tab without a retail
            // icon plays frame 1 instead of an unknown label.
            ("coremenu", "GetMenuIconName") => Ok(match self.tab(args.first()) {
                Some(t) if !t.icon.is_empty() => Value::Text(t.icon.clone()),
                _ => Value::Number(1.0),
            }),
            ("coremenu", "GetNumOptions") => Ok(Value::Number(
                self.tab(args.first()).map_or(0, |t| t.rows.len()) as f64,
            )),
            ("coremenu", "IsItemEnabled") => Ok(Value::Bool(
                self.row(&args).is_some_and(|r| r.enabled),
            )),
            ("coremenu", "GetOptionName") => Ok(Value::Text(
                self.row(&args).map_or_else(String::new, |r| r.label.clone()),
            )),
            ("coremenu", "GetOptionIconName") => Ok(match self.row(&args) {
                Some(r) if !r.icon.is_empty() => Value::Text(r.icon.clone()),
                _ => Value::Number(1.0),
            }),
            // NOT RETAIL (values): no sub-option lists, demo, DLC or career progression yet.
            ("coremenu", "GetMaxSubOptions") => Ok(Value::Number(0.0)),
            ("coremenu", "GetIsDemo") | ("coremenu", "IsDLCEnabled") => Ok(Value::Bool(false)),
            ("coremenu", "GetCurrentDLCBannerIndex") => Ok(Value::Number(0.0)),
            _ => {
                let types: Vec<_> = args.iter().map(kind).collect();
                *self
                    .unhandled
                    .entry(format!("{native}.{method}({})", types.join(", ")))
                    .or_default() += 1;
                Ok(Value::Undefined)
            }
        }
    }
}

pub struct Runtime {
    pub key: String,
    pub vm: Vm,
    pub host: MenuHost,
}

impl Runtime {
    /// Starts `movie`, a clone of `RetailMenuMovies::movies[key]` (linked, so imported characters
    /// and their action streams are already in it).
    pub fn start(key: &str, movie: Movie, crossbar: CrossBar) -> Result<Self, String> {
        let mut vm = Vm::new();
        for name in NATIVES {
            let object = vm.object(ObjectKind::Native(name.into()));
            let prototype = vm.object(ObjectKind::Plain);
            vm.set(object, "prototype", Value::Object(prototype))?;
            vm.set(vm.global, name, Value::Object(object))?;
        }
        let mut host = MenuHost {
            movie,
            random: 0x9e3779b9,
            screens: Vec::new(),
            crossbar,
            unhandled: BTreeMap::new(),
            traces: Vec::new(),
        };
        let initial: Vec<_> = host
            .movie
            .characters
            .values()
            .flat_map(|c| &c.frames)
            .flat_map(|f| &f.controls)
            .filter(|c| c.type_name == "do_init_action")
            .map(|c| c.actions_offset)
            .collect();
        for offset in initial {
            let code = host
                .movie
                .actions
                .get(&offset.to_string())
                .ok_or_else(|| format!("{key}: missing init action {offset}"))?
                .clone();
            vm.run(&code, &mut host)?;
        }
        vm.begin_update();
        host.movie.initialize(&mut vm)?;
        let mut runtime = Self {
            key: key.to_owned(),
            vm,
            host,
        };
        runtime.drain()?;
        Ok(runtime)
    }

    fn drain(&mut self) -> Result<(), String> {
        let mut calls = 0;
        while let Some(Pending { object, offset, .. }) = self.host.movie.next_action() {
            calls += 1;
            if calls > ACTION_LIMIT {
                return Err(format!("{}: frame action limit", self.key));
            }
            let code = self
                .host
                .movie
                .actions
                .get(&offset.to_string())
                .ok_or_else(|| format!("{}: missing action block {offset}", self.key))?
                .clone();
            self.vm.run_on(object, &code, &mut self.host)?;
        }
        Ok(())
    }

    /// Shows the registered screens the way the screen manager does once the game has the menu
    /// ready: `InitializeMenu` fills the crossbar from `coremenu` and plays `animatein` (the
    /// constructor stops the clip on `intro`, above the stage). Until milestone 5b answers
    /// `coremenu`, the menu fills from `undefined`. Returns how many screens were shown.
    pub fn show(&mut self) -> Result<usize, String> {
        self.vm.begin_update();
        let screens = self.host.screens.clone();
        for screen in &screens {
            self.vm
                .call_method(screen.controller, "InitializeMenu", vec![], &mut self.host)?;
            self.drain()?;
        }
        Ok(screens.len())
    }

    /// One movie frame: timeline advance, its frame actions and clip events, then collection.
    pub fn tick(&mut self) -> Result<(), String> {
        self.vm.begin_update();
        self.host.movie.advance(&mut self.vm)?;
        self.drain()?;
        self.vm
            .collect(self.host.movie.instances.keys().copied())?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retail_menu_movies::RetailMenuMovies;

    /// The pause menu ("crossbar"). Retail opens it from the pause button.
    const PAUSE_MENU: &str = "source/screens/main/core_menu";

    fn exported_set() -> Option<RetailMenuMovies> {
        let root = std::env::var("SKATE3_MENU_MOVIES")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| {
                crate::apt_imports::main_checkout().join("assets/private/menu-movies")
            });
        if !root.join("manifest.json").is_file() {
            eprintln!("skipped: no exported menu movies in {}", root.display());
            return None;
        }
        Some(RetailMenuMovies::read(&root).unwrap())
    }

    /// Runs the pause menu for two seconds of frames with the recording host and draws every
    /// frame. Prints what the movie asked the host for (the input to milestone 5b).
    #[test]
    fn pause_menu_runs_and_draws_with_the_recording_host() {
        let Some(set) = exported_set() else { return };
        let movie = set.movies[PAUSE_MENU].clone();
        // Item icons are labels of the crossbar's item icon sprite (menu-tables.json `items`).
        let row = |label: &str, icon: &str, enabled| CrossBarRow {
            label: label.into(),
            icon: icon.into(),
            enabled,
        };
        let crossbar = CrossBar {
            title: "ID_CROSSBAR_FREE_PLAY_MODE_TITLE".into(),
            tabs: vec![
                CrossBarTab {
                    label: "ID_CROSSBAR_SINGLE_PLAYER".into(),
                    // Category icons are labels of the crossbar's icon sprite (singleplayer,
                    // multiplayer, create, learn, options, debug), as in menu-tables.json.
                    icon: "singleplayer".into(),
                    rows: vec![
                        row("ID_CROSSBAR_FREESKATE", "freeskate", true),
                        row("ID_CROSSBAR_SKATEWITH", "skatewith", false),
                    ],
                },
                CrossBarTab {
                    label: "ID_CROSSBAR_OPTIONS".into(),
                    icon: "options".into(),
                    rows: vec![row("ID_CROSSBAR_GAMESETTINGS", "settings", true)],
                },
            ],
            menu: 0,
            option: 0,
        };
        let mut runtime = Runtime::start(PAUSE_MENU, movie, crossbar).unwrap();
        assert_eq!(runtime.host.screens.len(), 1, "core_menu registers one screen");
        match runtime.show() {
            Ok(shown) => assert_eq!(shown, 1),
            Err(e) => {
                for (call, count) in &runtime.host.unhandled {
                    eprintln!("  unhandled {count:>4} x {call}");
                }
                panic!("show: {e}");
            }
        }
        let mut draws = 0;
        for frame in 0..60 {
            runtime
                .tick()
                .unwrap_or_else(|e| panic!("frame {frame}: {e}"));
            let scene = set.scene(PAUSE_MENU, &runtime.host.movie);
            draws = match crate::apt_scene::draw(&runtime.host.movie, &runtime.vm, &scene) {
                Ok(d) => d.len(),
                Err(e) => {
                    for (call, count) in &runtime.host.unhandled {
                        eprintln!("  unhandled {count:>4} x {call}");
                    }
                    panic!("frame {frame} draw: {e}")
                }
            };
        }
        eprintln!(
            "RETAIL_MENU_RUNTIME {PAUSE_MENU} draws_after_60_frames={draws} unhandled={} traces={}",
            runtime.host.unhandled.len(),
            runtime.host.traces.len()
        );
        for (call, count) in &runtime.host.unhandled {
            eprintln!("  unhandled {count:>4} x {call}");
        }
        for trace in runtime.host.traces.iter().take(20) {
            eprintln!("  trace {trace}");
        }
        let scene = set.scene(PAUSE_MENU, &runtime.host.movie);
        let final_draws = crate::apt_scene::draw(&runtime.host.movie, &runtime.vm, &scene).unwrap();
        // Before `show` the clip sat on `intro`, every draw above the stage (y < 0).
        let on_stage = final_draws
            .iter()
            .filter(|d| d.vertices.iter().any(|v| (0.0..720.0).contains(&v.position[1])))
            .count();
        eprintln!("  on_stage={on_stage} of {}", final_draws.len());
        assert!(on_stage > 0, "the shown pause menu draws nothing on the 1280 x 720 stage");
        for draw in final_draws {
            let (mut lo, mut hi) = ([f32::MAX; 2], [f32::MIN; 2]);
            for v in &draw.vertices {
                for axis in 0..2 {
                    lo[axis] = lo[axis].min(v.position[axis]);
                    hi[axis] = hi[axis].max(v.position[axis]);
                }
            }
            eprintln!(
                "  draw alpha={:.2} rgb=({:.2},{:.2},{:.2}) x={:.0}..{:.0} y={:.0}..{:.0} {:?} {}",
                draw.multiply[3], draw.multiply[0], draw.multiply[1], draw.multiply[2],
                lo[0], hi[0], lo[1], hi[1], draw.fill, draw.texture
            );
        }
        assert!(draws > 0, "the pause menu draws nothing");
    }
}
