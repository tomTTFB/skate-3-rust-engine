//! Retail pause menu drawn in game (doc 31, milestone 5a), behind the dev flag
//! `SKATE_RETAIL_MENU=1`. F8 opens and closes it over the game; the current overlay keeps
//! Escape / Start until the retail menus can replace it (milestone 5d).
//!
//! The movie runs in `retail_menu_runtime` and draws like the scoring HUD: an offscreen target in
//! the movie's 1280 x 720 stage space at window resolution, one 2D mesh per APT draw with the HUD
//! material (`texel * multiply + add`), composited over the frame by a UI node. Menu units already
//! carry retail's single unit colour in `multiply` (`apt_scene`, 825D5580), so solid fills sample a
//! 1 x 1 white texture. Textures resolve through `RetailMenuMovies::image` (retail payloads, mod
//! bitmaps) and the movie's fonts. It ticks on real time, since menus animate while play is paused.
//!
//! Not yet: host calls the movie makes are answered `undefined` and logged (milestone 5b), input
//! does not reach the movie (5c), and a window resize keeps the target size chosen at startup.
use crate::{
    apt_scene::{self, Fill},
    config::Config,
    retail_menu_movies::{MenuImage, RetailMenuMovies},
    retail_menu_runtime::{CrossBar, CrossBarRow, CrossBarTab, Runtime},
    retail_menus::RetailMenus,
    scoring_hud::{ColorTransform, HudComposite, HudMaterial},
};
use skate_core::menus::{Icon, MenuContext, MenuRegistry};
use bevy::{
    asset::RenderAssetUsages,
    camera::{RenderTarget, ScalingMode, visibility::RenderLayers},
    image::{ImageAddressMode, ImageSampler, ImageSamplerDescriptor},
    prelude::*,
    render::render_resource::{Extent3d, PrimitiveTopology, TextureDimension, TextureFormat},
    window::PrimaryWindow,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

/// The pause menu ("crossbar").
const PAUSE_MENU: &str = "source/screens/main/core_menu";
/// Movie header `milliseconds_per_frame` of the retail menu movies.
const FRAME_SECONDS: f32 = 0.016;
/// Frames run per rendered frame at most, so a stall does not replay seconds of animation.
const MAX_FRAMES_PER_UPDATE: u32 = 4;
/// Free render layer (28 mods / character, 29 session marker, 31 HUD).
const LAYER: usize = 30;

pub(crate) fn enabled() -> bool {
    std::env::var_os("SKATE_RETAIL_MENU").is_some_and(|v| v != "0")
}

/// The crossbar `coremenu` answers for `context`: the registry's visible tabs of the mode and
/// their visible rows (mod edits and greying rules applied), labelled with language-table ids.
pub(crate) fn crossbar(registry: &MenuRegistry, context: &MenuContext) -> CrossBar {
    let data = registry.data();
    let tabs = registry
        .tabs(context)
        .into_iter()
        .map(|category| CrossBarTab {
            label: data
                .category(&category)
                .map_or_else(|| category.clone(), |c| c.label.clone()),
            icon: match data.category(&category).map(|c| &c.icon) {
                Some(Icon::Retail(label)) => label.clone(),
                _ => String::new(),
            },
            rows: registry
                .tab_rows(&category, context)
                .into_iter()
                .map(|row| CrossBarRow {
                    label: data.item(&row.id).map_or_else(|| row.id.clone(), |i| i.label.clone()),
                    icon: match data.item(&row.id).map(|i| &i.icon) {
                        Some(Icon::Retail(label)) => label.clone(),
                        _ => String::new(),
                    },
                    enabled: row.enabled,
                })
                .collect(),
        })
        .collect();
    CrossBar {
        title: data
            .mode(&context.mode)
            .map_or_else(String::new, |m| m.title.clone()),
        tabs,
        menu: 0,
        option: 0,
    }
}

struct Slot {
    entity: Entity,
    mesh: Handle<Mesh>,
    material: Handle<HudMaterial>,
}

#[derive(Resource)]
struct MenuScreen {
    root: PathBuf,
    runtime: Option<Runtime>,
    node: Entity,
    white: Handle<Image>,
    /// (texture key, wrapped) -> image; "" is the white texture.
    textures: BTreeMap<(String, bool), Handle<Image>>,
    /// Texture keys that could not be resolved, logged once.
    missing: BTreeSet<String>,
    /// Unhandled host calls already logged.
    reported: BTreeSet<String>,
    slots: Vec<Slot>,
    shown: usize,
    elapsed: f32,
}

pub(crate) struct RetailMenuScreenPlugin;

impl Plugin for RetailMenuScreenPlugin {
    fn build(&self, app: &mut App) {
        if !enabled() {
            return;
        }
        app.add_systems(
            PostStartup,
            setup.after(crate::graphics_menu::PresentationSetup),
        )
        .add_systems(Update, (toggle, tick, render).chain());
    }
}

fn setup(
    mut commands: Commands,
    config: Res<Config>,
    cameras: Query<Entity, With<IsDefaultUiCamera>>,
    window: Single<&Window, With<PrimaryWindow>>,
    mut images: ResMut<Assets<Image>>,
    mut composites: ResMut<Assets<HudComposite>>,
) {
    let Ok(output) = cameras.single() else {
        error!("RETAIL_MENU requires the presentation camera");
        return;
    };
    let white = images.add(Image::new(
        Extent3d {
            width: 1,
            height: 1,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        vec![255; 4],
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    ));
    let target = images.add(Image::new_target_texture(
        window.physical_width().max(1),
        window.physical_height().max(1),
        TextureFormat::Rgba8UnormSrgb,
        None,
    ));
    commands.spawn((
        Camera2d,
        Projection::Orthographic(OrthographicProjection {
            scaling_mode: ScalingMode::Fixed {
                width: 1280.,
                height: 720.,
            },
            ..OrthographicProjection::default_2d()
        }),
        Camera {
            order: -2,
            clear_color: ClearColorConfig::Custom(Color::NONE),
            ..default()
        },
        RenderTarget::Image(target.clone().into()),
        RenderLayers::layer(LAYER),
        Msaa::Off,
    ));
    let node = commands
        .spawn((
            MaterialNode(composites.add(HudComposite { image: target })),
            UiTargetCamera(output),
            // Above the scoring HUD (1).
            GlobalZIndex(2),
            Pickable::IGNORE,
            Visibility::Hidden,
            Node {
                position_type: PositionType::Absolute,
                width: percent(100),
                height: percent(100),
                ..default()
            },
        ))
        .id();
    commands.insert_resource(MenuScreen {
        root: config.asset_root.join("private/menu-movies"),
        runtime: None,
        node,
        white,
        textures: BTreeMap::new(),
        missing: BTreeSet::new(),
        reported: BTreeSet::new(),
        slots: Vec::new(),
        shown: 0,
        elapsed: 0.,
    });
    info!("RETAIL_MENU enabled: F8 opens the retail pause menu");
}

fn toggle(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    screen: Option<ResMut<MenuScreen>>,
    movies: Res<RetailMenuMovies>,
    menus: Res<RetailMenus>,
) {
    let Some(mut screen) = screen else { return };
    if !keys.just_pressed(KeyCode::F8) {
        return;
    }
    if screen.runtime.take().is_some() {
        commands.entity(screen.node).insert(Visibility::Hidden);
        info!("RETAIL_MENU closed");
        return;
    }
    let Some(movie) = movies.movies.get(PAUSE_MENU) else {
        warn!("RETAIL_MENU {PAUSE_MENU} is not in the menu movie set");
        return;
    };
    let bar = crossbar(&menus.registry, &menus.context);
    let summary = format!(
        "mode={} title={} tabs={} rows={}",
        menus.context.mode,
        bar.title,
        bar.tabs.len(),
        bar.tabs.iter().map(|t| t.rows.len()).sum::<usize>()
    );
    // The constructor stops the clip on `intro` (above the stage) and registers it; `show` is
    // the screen manager's InitializeMenu, which fills the crossbar and plays `animatein`.
    match Runtime::start(PAUSE_MENU, movie.clone(), bar).and_then(|mut runtime| {
        runtime.show()?;
        Ok(runtime)
    }) {
        Ok(runtime) => {
            screen.runtime = Some(runtime);
            screen.elapsed = 0.;
            commands.entity(screen.node).insert(Visibility::Visible);
            info!("RETAIL_MENU opened {PAUSE_MENU} {summary}");
        }
        Err(error) => error!("RETAIL_MENU {PAUSE_MENU} could not start: {error}"),
    }
}

fn tick(mut commands: Commands, time: Res<Time<Real>>, screen: Option<ResMut<MenuScreen>>) {
    let Some(mut screen) = screen else { return };
    let screen = &mut *screen;
    let Some(runtime) = screen.runtime.as_mut() else {
        return;
    };
    screen.elapsed += time.delta_secs();
    let mut frames = 0;
    while screen.elapsed >= FRAME_SECONDS && frames < MAX_FRAMES_PER_UPDATE {
        screen.elapsed -= FRAME_SECONDS;
        frames += 1;
        if let Err(error) = runtime.tick() {
            error!("RETAIL_MENU {} stopped: {error}", runtime.key);
            screen.runtime = None;
            commands.entity(screen.node).insert(Visibility::Hidden);
            return;
        }
    }
    screen.elapsed = screen.elapsed.min(FRAME_SECONDS);
    for call in runtime.host.unhandled.keys() {
        if screen.reported.insert(call.clone()) {
            info!("RETAIL_MENU unhandled host call {call}");
        }
    }
}

/// The image behind a draw's texture key, created once per key and addressing mode.
fn texture(
    screen: &mut MenuScreen,
    images: &mut Assets<Image>,
    movies: &RetailMenuMovies,
    key: &str,
    fill: Fill,
) -> Option<Handle<Image>> {
    if key.is_empty() {
        return Some(screen.white.clone());
    }
    let wrapped = fill == Fill::TextureWrapped;
    if let Some(handle) = screen.textures.get(&(key.to_owned(), wrapped)) {
        return Some(handle.clone());
    }
    if screen.missing.contains(key) {
        return None;
    }
    let loaded = (|| -> Result<(Vec<u8>, [u32; 2]), String> {
        let (path, size) = match movies.image(key) {
            Some(MenuImage::Mod(image)) => return Ok((image.rgba.clone(), [image.width, image.height])),
            Some(MenuImage::Retail { path, width, height }) => (path.to_owned(), [width, height]),
            // Text draws name a font atlas of the running movie.
            None => {
                let runtime = screen.runtime.as_ref().ok_or("no running menu")?;
                let fonts = &runtime.host.movie.text_assets.fonts;
                fonts
                    .values()
                    .flat_map(|f| std::iter::once(f).chain(f.foreground.as_deref()))
                    .find(|f| f.texture == key)
                    .map(|f| (f.texture.clone(), f.size))
                    .ok_or("not a menu payload or font atlas")?
            }
        };
        let bytes = std::fs::read(screen.root.join(&path)).map_err(|e| e.to_string())?;
        if bytes.len() != size[0] as usize * size[1] as usize * 4 {
            return Err(format!("size is not {}x{} RGBA8", size[0], size[1]));
        }
        Ok((bytes, size))
    })();
    let (bytes, [width, height]) = match loaded {
        Ok(loaded) => loaded,
        Err(error) => {
            warn!("RETAIL_MENU texture {key}: {error}");
            screen.missing.insert(key.to_owned());
            return None;
        }
    };
    let mut image = Image::new(
        Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        bytes,
        TextureFormat::Rgba8UnormSrgb,
        RenderAssetUsages::RENDER_WORLD,
    );
    let mut sampler = ImageSamplerDescriptor::linear();
    if wrapped {
        sampler.address_mode_u = ImageAddressMode::Repeat;
        sampler.address_mode_v = ImageAddressMode::Repeat;
    }
    image.sampler = ImageSampler::Descriptor(sampler);
    let handle = images.add(image);
    screen
        .textures
        .insert((key.to_owned(), wrapped), handle.clone());
    Some(handle)
}

fn render(
    mut commands: Commands,
    screen: Option<ResMut<MenuScreen>>,
    movies: Res<RetailMenuMovies>,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<HudMaterial>>,
) {
    let Some(mut screen) = screen else { return };
    let screen = &mut *screen;
    let draws = match screen.runtime.as_ref() {
        Some(runtime) => {
            let scene = movies.scene(&runtime.key, &runtime.host.movie);
            match apt_scene::draw(&runtime.host.movie, &runtime.vm, &scene) {
                Ok(draws) => draws,
                Err(error) => {
                    error!("RETAIL_MENU geometry: {error}");
                    screen.runtime = None;
                    commands.entity(screen.node).insert(Visibility::Hidden);
                    Vec::new()
                }
            }
        }
        None => Vec::new(),
    };
    let mut used = 0;
    for draw in &draws {
        let Some(atlas) = texture(screen, &mut images, &movies, &draw.texture, draw.fill) else {
            continue;
        };
        let mut mesh = Mesh::new(
            PrimitiveTopology::TriangleList,
            RenderAssetUsages::MAIN_WORLD | RenderAssetUsages::RENDER_WORLD,
        );
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_POSITION,
            draw.vertices
                .iter()
                .map(|v| [v.position[0] - 640., 360. - v.position[1], 0.])
                .collect::<Vec<_>>(),
        );
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_UV_0,
            draw.vertices.iter().map(|v| v.uv).collect::<Vec<_>>(),
        );
        mesh.insert_attribute(
            Mesh::ATTRIBUTE_NORMAL,
            vec![[0., 0., 1.]; draw.vertices.len()],
        );
        let material = HudMaterial {
            color: ColorTransform {
                multiply: draw.multiply.into(),
                add: draw.add.into(),
            },
            atlas,
        };
        if used == screen.slots.len() {
            let mesh = meshes.add(mesh);
            let material = materials.add(material);
            let entity = commands
                .spawn((
                    Mesh2d(mesh.clone()),
                    MeshMaterial2d(material.clone()),
                    Transform::from_xyz(0., 0., used as f32 * 0.01),
                    RenderLayers::layer(LAYER),
                ))
                .id();
            screen.slots.push(Slot {
                entity,
                mesh,
                material,
            });
        } else {
            let slot = &screen.slots[used];
            if let Some(old) = meshes.get_mut(&slot.mesh) {
                *old = mesh;
            }
            if let Some(old) = materials.get_mut(&slot.material) {
                *old = material;
            }
            if used >= screen.shown {
                commands.entity(slot.entity).insert(Visibility::Visible);
            }
        }
        used += 1;
    }
    for slot in screen.slots.iter().take(screen.shown).skip(used) {
        commands.entity(slot.entity).insert(Visibility::Hidden);
    }
    screen.shown = used;
}

#[cfg(test)]
mod tests {
    use super::*;
    use skate_core::menus::{Category, Item, MenuData, Mode, Tab};

    fn data() -> MenuData {
        let category = |id: &str, icon: &str| Category {
            id: id.into(),
            label: format!("ID_CROSSBAR_{}", id.to_uppercase()),
            icon: Icon::Retail(icon.into()),
        };
        let item = |id: &str, icon: Icon| Item {
            id: id.into(),
            label: format!("ID_CROSSBAR_{}", id.to_uppercase()),
            icon,
            help: String::new(),
            sub_option_kind: -1,
        };
        MenuData {
            categories: vec![category("SinglePlayer", "singleplayer"), category("Options", "options")],
            items: vec![
                item("SoloFreeskate", Icon::Retail("solofreesakte".into())),
                item("ModThing", Icon::Image("thing.png".into())),
                item("GameSettings", Icon::Retail("settings".into())),
            ],
            modes: vec![Mode {
                key: "FreePlay".into(),
                title: "ID_CROSSBAR_FREE_PLAY_MODE_TITLE".into(),
                slots: 8,
                tabs: vec![
                    Tab {
                        category: "SinglePlayer".into(),
                        items: vec!["SoloFreeskate".into(), "ModThing".into()],
                    },
                    Tab {
                        category: "Options".into(),
                        items: vec!["GameSettings".into()],
                    },
                ],
            }],
            ..MenuData::default()
        }
    }

    #[test]
    fn crossbar_reads_the_registry_view_of_the_mode() {
        let registry = MenuRegistry::new(data());
        let bar = crossbar(&registry, &MenuContext::new("FreePlay"));
        assert_eq!(bar.title, "ID_CROSSBAR_FREE_PLAY_MODE_TITLE");
        assert_eq!((bar.menu, bar.option), (0, 0));
        let tabs: Vec<_> = bar
            .tabs
            .iter()
            .map(|t| (t.label.as_str(), t.icon.as_str(), t.rows.len()))
            .collect();
        assert_eq!(
            tabs,
            [
                ("ID_CROSSBAR_SINGLEPLAYER", "singleplayer", 2),
                ("ID_CROSSBAR_OPTIONS", "options", 1)
            ]
        );
        let first = &bar.tabs[0].rows;
        assert_eq!((first[0].label.as_str(), first[0].icon.as_str()), ("ID_CROSSBAR_SOLOFREESKATE", "solofreesakte"));
        // A mod image is not a sprite label: the host plays frame 1 for it.
        assert_eq!(first[1].icon, "");
        // Retail rule: every entry is greyed until a feature registers a handler for it.
        assert!(bar.tabs.iter().flat_map(|t| &t.rows).all(|r| !r.enabled));
    }

    #[test]
    fn crossbar_of_an_unknown_mode_is_empty() {
        let registry = MenuRegistry::new(data());
        let bar = crossbar(&registry, &MenuContext::new("NoSuchMode"));
        assert_eq!(bar.title, "");
        assert!(bar.tabs.is_empty());
    }
}
