use bevy::prelude::*;
use bevy::window::PresentMode;

mod webrtc_client_plugin;
use webrtc_client_plugin::WebRTCClientPlugin;

fn main() {
    App::new()
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                present_mode: PresentMode::Immediate,
                ..default()
            }),
            ..default()
        }))
        .add_plugins(WebRTCClientPlugin)
        .run();
}
