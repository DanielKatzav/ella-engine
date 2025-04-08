mod gstreamer_plugin;
mod webrtc_plugin;

use crate::gstreamer_plugin::GStreamerPlugin;
use bevy::prelude::*;
use bevy::window::PresentMode;
use crate::webrtc_plugin::WebRTCServerPlugin;

#[derive(Component)]
struct VideoImage {
    handle: Handle<Image>,
}

fn main() {
    App::new()
        // Ensure you bring in the DefaultPlugins which include the asset system.
        .add_plugins(DefaultPlugins.set(WindowPlugin {
            primary_window: Some(Window {
                // Try using Mailbox or Immediate.
                present_mode: PresentMode::Immediate,
                ..Default::default()
            }),
            ..Default::default()
        }))
        // Schedule the startup system using the new API.
        .add_plugins(GStreamerPlugin)
        .add_plugins(WebRTCServerPlugin)
        .run();
}
