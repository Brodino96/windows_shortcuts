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
use winit::event::Event as WinitEvent;
use winit::event_loop::{ControlFlow, EventLoopBuilder};

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

fn main() -> Result<()> {
    SimpleLogger::new()
        .with_level(LevelFilter::Info)
        .init()?;
    info!("Starting application");

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build()?;
    let proxy = event_loop.create_proxy();
    let manager = GlobalHotKeyManager::new().context("Failed to create hotkey manager")?;
    let mut hotkeys_map: HashMap<u32, (HotKey, String)> = HashMap::new();

    let mut watcher = recommended_watcher(move |res| {
        if let Ok(Event { kind, .. }) = res {
            if kind.is_modify() || kind.is_create() {
                proxy.send_event(UserEvent::ConfigChanged).unwrap();
            }
        }
    })?;

    let config_path = get_config_path()?;
    watcher.watch(&config_path, RecursiveMode::NonRecursive)?;

    load_and_register_hotkeys(&manager, &mut hotkeys_map, &config_path)?;

    info!("Listening for hotkeys and config changes...");
    let hotkey_receiver = GlobalHotKeyEvent::receiver();

    event_loop.run(move |event, elwt| {
        elwt.set_control_flow(ControlFlow::Wait);

        match event {
            WinitEvent::UserEvent(UserEvent::ConfigChanged) => {
                info!("Config file changed. Reloading hotkeys...");
                let keys_to_unregister: Vec<HotKey> = hotkeys_map.values().map(|(k, _)| *k).collect();
                if !keys_to_unregister.is_empty() {
                    if let Err(e) = manager.unregister_all(&keys_to_unregister) {
                        error!("Failed to unregister all hotkeys: {}", e);
                    }
                }
                hotkeys_map.clear();
                if let Err(e) = load_and_register_hotkeys(&manager, &mut hotkeys_map, &config_path) {
                    error!("Failed to reload and register hotkeys: {}", e);
                }
            }
            WinitEvent::AboutToWait => {
                if let Ok(event) = hotkey_receiver.try_recv() {
                    if event.state == HotKeyState::Pressed {
                        if let Some((_, path)) = hotkeys_map.get(&event.id) {
                            info!("Hotkey {} pressed, opening: {}", event.id, path);
                            if let Err(e) = open::that(path) {
                                error!("Failed to open path \'{}\": {}", path, e);
                            }
                        }
                    }
                }
            }
            _ => (),
        }
    })
    .context("Event loop failed")
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