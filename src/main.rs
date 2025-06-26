use anyhow::{anyhow, Context, Result};
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use log::{error, info, LevelFilter};
use notify::{recommended_watcher, Event, RecursiveMode, Watcher};
use open;
use serde::Deserialize;
use simple_logger::SimpleLogger;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use winit::event_loop::{ControlFlow, EventLoop, ActiveEventLoop};
use winit::application::ApplicationHandler;
use tray_icon::{menu::{Menu, MenuItem, MenuEvent, MenuId}, TrayIconBuilder, TrayIconEvent, Icon};

#[derive(Debug, Deserialize, Clone)]
struct Config {
    hotkeys: Vec<HotkeyConfig>,
}

#[derive(Debug, Deserialize, Clone)]
struct HotkeyConfig {
    shortcut: String,
    path: String,
}

#[derive(Debug, Clone, Copy)]
enum UserEvent {
    ConfigChanged,
}



struct App {
    manager: GlobalHotKeyManager,
    hotkeys_map: HashMap<u32, (HotKey, String)>,
    config_path: PathBuf,
    quit_item_id: MenuId,
}

impl ApplicationHandler<UserEvent> for App {
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, event: UserEvent) {
        match event {
            UserEvent::ConfigChanged => {
                info!("Config file changed. Reloading hotkeys...");
                let keys_to_unregister: Vec<HotKey> = self.hotkeys_map.values().map(|(k, _)| *k).collect();
                if !keys_to_unregister.is_empty() {
                    if let Err(e) = self.manager.unregister_all(&keys_to_unregister) {
                        error!("Failed to unregister all hotkeys: {}", e);
                    }
                }
                self.hotkeys_map.clear();
                if let Err(e) = load_and_register_hotkeys(&self.manager, &mut self.hotkeys_map, &self.config_path) {
                    error!("Failed to reload and register hotkeys: {}", e);
                }
            }
        }
    }

    fn resumed(&mut self, _event_loop: &ActiveEventLoop) {
        // This is where you'd typically create windows, but we don't have any.
        // We need to ensure the event loop stays alive.
    }

    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        _event: winit::event::WindowEvent,
    ) {
        // No windows in this application, so nothing to do here.
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, _cause: winit::event::StartCause) {
        event_loop.set_control_flow(ControlFlow::Wait);

        if let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if event.state == HotKeyState::Pressed {
                if let Some((_, path)) = self.hotkeys_map.get(&event.id) {
                    info!("Hotkey {} pressed, opening: {}", event.id, path);
                    if let Err(e) = open::that(path) {
                        error!("Failed to open path \'{}\': {}", path, e);
                    }
                }
            }
        }
        if let Ok(event) = TrayIconEvent::receiver().try_recv() {
            info!("Tray event received: {:?}, quit_item id: {:?}", event, self.quit_item_id);
            if event.id().0 == self.quit_item_id.0 {
                info!("Quit item clicked, exiting application.");
                event_loop.exit();
            }
        }
        if let Ok(event) = MenuEvent::receiver().try_recv() {
            info!("Menu event received: {:?}, quit_item id: {:?}", event, self.quit_item_id);
            if *event.id() == self.quit_item_id {
                info!("Quit item clicked, exiting application.");
                event_loop.exit();
            }
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        // This is called when the event loop is about to go to sleep until new events arrive.
        // We can use this to poll for events from other sources.
    }

    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        info!("Exiting application.");
    }
}

fn main() -> Result<()> {
    SimpleLogger::new()
        .with_level(LevelFilter::Info)
        .init()?;
    info!("Starting application");

    create_startup_shortcut().context("Failed to create startup shortcut")?;

    let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
    let manager = GlobalHotKeyManager::new().context("Failed to create hotkey manager")?;
    let mut hotkeys_map: HashMap<u32, (HotKey, String)> = HashMap::new();

    let config_path = get_config_path()?;
    load_and_register_hotkeys(&manager, &mut hotkeys_map, &config_path)?;

    let tray_menu = Menu::new();
    let quit_item = MenuItem::new("Quit", true, None);
    let quit_item_id = quit_item.id();
    let _ = tray_menu.append_items(&[&quit_item]);

    let icon_bytes = include_bytes!("../assets/icon.png");
    
    let (icon_rgba, icon_width, icon_height) = {
        let image = image::load_from_memory(icon_bytes)
            .expect("Failed to open icon path")
            .into_rgba8();
        let (width, height) = image.dimensions();
        let rgba = image.into_raw();
        (rgba, width, height)
    };
    let icon = tray_icon::Icon::from_rgba(icon_rgba, icon_width, icon_height).expect("Failed to open icon");

    let _tray_icon = Some(TrayIconBuilder::new()
        .with_menu(Box::new(tray_menu))
        .with_tooltip("Windows Shortcuts")
        .with_icon(icon)
        .build()?);

    info!("Listening for hotkeys and config changes...");

    let mut app = App {
        manager,
        hotkeys_map,
        config_path: config_path.clone(),
        quit_item_id: quit_item_id.clone(),
    };

    let proxy = event_loop.create_proxy();
    let mut watcher = recommended_watcher(move |res| {
        if let Ok(Event { kind, .. }) = res {
            if kind.is_modify() || kind.is_create() {
                proxy.send_event(UserEvent::ConfigChanged).unwrap();
            }
        }
    })?;
    watcher.watch(&config_path, RecursiveMode::NonRecursive)?;

    event_loop.run_app(&mut app).unwrap();

    Ok(())
}

fn load_and_register_hotkeys(
    manager: &GlobalHotKeyManager,
    hotkeys_map: &mut HashMap<u32, (HotKey, String)>,
    config_path: &Path,
) -> Result<()> {
    let config = load_or_create_config(config_path)?;
    for hotkey_config in config.hotkeys {
        match HotKey::from_str(&hotkey_config.shortcut) {
            Ok(hotkey) => {
                if let Err(e) = manager.register(hotkey) {
                    error!("Failed to register hotkey for shortcut \'{}\": {}", hotkey_config.shortcut, e);
                    continue;
                }
                hotkeys_map.insert(hotkey.id(), (hotkey, hotkey_config.path.clone()));
                info!("Registered hotkey: {} for path {}", hotkey_config.shortcut, hotkey_config.path);
            }
            Err(e) => {
                error!("Failed to parse shortcut \'{}\": {}", hotkey_config.shortcut, e);
            }
        }
    }
    Ok(())
}

fn get_config_path() -> Result<PathBuf> {
    let config_dir = dirs::config_dir().ok_or_else(|| anyhow!("Could not find config directory"))?;
    let app_config_dir = config_dir.join("WindowsShortcuts");
    fs::create_dir_all(&app_config_dir).context("Failed to create app config directory")?;
    Ok(app_config_dir.join("config.toml"))
}

fn load_or_create_config(config_path: &Path) -> Result<Config> {
    if !config_path.exists() {
        info!("Config file not found, creating a default one at {:?}", config_path);
        let default_config_content = r#"
# Example hotkeys. Use Ctrl, Shift, Alt, Win as modifiers.
# Keys can be A-Z, 0-9, F1-F12.
# For file paths, it is recommended to use forward slashes (e.g., "C:/Users/YourUser/Documents/file.txt")
# or double backslashes (e.g., "C:\\Users\\YourUser\\Documents\\file.txt").

[[hotkeys]]
shortcut = "Ctrl+Shift+A"
path = "C:/Windows/System32/notepad.exe"

[[hotkeys]]
shortcut = "Ctrl+Shift+B"
path = "https://www.google.com"
"#;
        fs::write(&config_path, default_config_content).context("Failed to write default config")?;
    }

    let config_content = fs::read_to_string(&config_path).context("Failed to read config file")?;
    toml::from_str(&config_content).context("Failed to parse TOML config")
}

#[cfg(target_os = "windows")]
fn create_startup_shortcut() -> Result<()> {
    use std::env;
use shortcuts_rs::ShellLink;

    let app_exe = env::current_exe().context("Failed to get current executable path")?;
    let app_name = app_exe.file_stem()
        .and_then(|s| s.to_str())
        .context("Failed to get app name")?;

    let startup_dir = dirs::config_dir()
        .ok_or_else(|| anyhow!("Could not find config directory"))?
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join("Startup");

    fs::create_dir_all(&startup_dir).context("Failed to create startup directory")?;

    let shortcut_path = startup_dir.join(format!("{}.lnk", app_name));

    if !shortcut_path.exists() {
        info!("Creating startup shortcut at {:?}", shortcut_path);
        let shortcut = ShellLink::new(&app_exe, None, None, None)?;
        shortcut.create_lnk(&shortcut_path)?;
    } else {
        info!("Startup shortcut already exists at {:?}", shortcut_path);
    }

    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn create_startup_shortcut() -> Result<()> {
    info!("Startup shortcut creation is only supported on Windows.");
    Ok(())
}
