// gstreamer_plugin.rs
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat, TextureUsages};

use crossbeam_channel::{unbounded, Receiver};
use gstreamer as gst;
use gstreamer::prelude::{Cast, ElementExt, GstBinExt, GstObjectExt};
use gstreamer_app::{AppSink, AppSinkCallbacks};
use std::thread;

/// Resource that holds a receiver for raw video frames (as RGBA bytes)
#[derive(Resource)]
pub struct GstReceiver {
    pub receiver: Receiver<Vec<u8>>,
}

/// Marker component for the sprite that will display the video.
#[derive(Component)]
pub struct VideoSprite;
#[derive(Component)]
pub struct VideoHandle {
    pub handle: Handle<Image>,
}
/// The Bevy plugin that integrates GStreamer video into your application.
#[derive(Resource)]
pub struct GStreamerPlugin;
impl Plugin for GStreamerPlugin {
    fn build(&self, app: &mut App) {
        // --- GStreamer Initialization & Pipeline Setup ---
        // Initialize GStreamer and create an mpsc channel for frame data.
        gst::init().expect("Failed to initialize GStreamer");
        let (rgba_tx, rgba_rx) = unbounded();

        // Spawn the GStreamer pipeline on a separate thread.
        thread::spawn(move || {
            // Here we use videotestsrc for demonstration.
            // Replace the pipeline string with your video source if needed.
            // Updated pipeline string for the WebRTC branch:
            let pipeline_str = "videotestsrc is-live=true ! video/x-raw,format=RGBA,width=640,height=480,framerate=30/1 ! appsink name=local_sink";

            let pipeline =
                gst::parse::launch(pipeline_str).expect("Failed to create GStreamer pipeline");

            // Retrieve the appsink element from the pipeline.
            let bin = pipeline
                .clone()
                .dynamic_cast::<gst::Bin>()
                .expect("Pipeline is not a Bin");

            let appsink = bin
                .by_name("local_sink")
                .expect("Sink element not found")
                .dynamic_cast::<AppSink>()
                .expect("Sink element is not an AppSink");

            // Instead of the old connect method, use set_callbacks with AppSinkCallbacks.
            appsink.set_callbacks(
                AppSinkCallbacks::builder()
                    .new_sample(move |appsink| {
                        // Pull the sample from the appsink.
                        if let Ok(sample) = appsink.pull_sample() {
                            if let Some(buffer) = sample.buffer() {
                                if let Ok(map) = buffer.map_readable() {
                                    // Clone the frame data to a vector.
                                    let data = map.as_slice().to_vec();
                                    // Send the frame data. (If the channel is closed, ignore the error.)
                                    debug!("New sample from Gstreamer frame with {} bytes", data.len());
                                    let _ = rgba_tx.send(data);
                                }
                            }
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );


            // Set the pipeline to the Playing state.
            pipeline
                .set_state(gst::State::Playing)
                .expect("Unable to set the pipeline to the Playing state");

            // Wait for End-Of-Stream or an error.
            let bus = pipeline.bus().expect("Pipeline has no bus");
            for msg in bus.iter_timed(gst::ClockTime::NONE) {
                use gst::MessageView;
                match msg.view() {
                    MessageView::Eos(..) => break,
                    MessageView::Error(err) => {
                        eprintln!(
                            "Error from {:?}: {} ({:?})",
                            msg.src().map(|s| s.path_string()),
                            err.error(),
                            err.debug()
                        );
                        break;
                    }
                    _ => {}
                }
            }
            // Clean up the pipeline.
            pipeline
                .set_state(gst::State::Null)
                .expect("Unable to set the pipeline to the Null state");
        });

        // --- Bevy Plugin Setup ---
        // Insert the GstReceiver as a resource so that other systems can access the frames.
        app.insert_resource(GstReceiver { receiver: rgba_rx })
            // Spawn the video sprite and camera during startup.
            .add_systems(Startup, Self::setup_video_sprite_once)
            // Add the update system that polls for new frames and updates the texture.
            .add_systems(Update, Self::video_update);
    }
}

impl GStreamerPlugin {
    fn setup_video_sprite_once(
        mut commands: Commands,
        mut images: ResMut<Assets<Image>>,
        mut setup_done: Local<bool>,
    ) {
        if *setup_done {
            return;
        }
        // Define texture dimensions matching the GStreamer pipeline.
        let width = 640;
        let height = 480;
        let texture_size = Extent3d {
            width,
            height,
            depth_or_array_layers: 1,
        };

        // Create a dynamic texture (initialized with opaque black).
        let mut image = Image::new_fill(
            texture_size,
            TextureDimension::D2,
            &[0, 0, 0, 255],
            TextureFormat::Rgba8UnormSrgb,
            Default::default(),
        );
        image.texture_descriptor.usage = TextureUsages::TEXTURE_BINDING | TextureUsages::COPY_DST;

        // Add the image asset.
        let image_handle = images.add(image);

        // Spawn the sprite that will display the video.
        commands
            .spawn(Sprite {
                image: image_handle.clone(),
                ..Default::default()
            })
            .insert(VideoSprite)
            .insert(VideoHandle {
                handle: image_handle,
            });

        // Also spawn a default 2D camera.
        commands.spawn(Camera2d::default());

        *setup_done = true;
    }

    fn video_update(
        mut images: ResMut<Assets<Image>>,
        gst_receiver: Res<GstReceiver>,
        query: Query<&VideoHandle, With<VideoSprite>>,
    ) {
        // Attempt to retrieve a frame from the GStreamer channel.
        if let Ok(frame_data) = gst_receiver.receiver.try_recv() {
            debug!(
                "Updating texture with frame of size {} bytes",
                frame_data.len()
            );
            // For each sprite marked with VideoSprite, update its texture.
            for image_handle in query.iter() {
                if let Some(image) = images.get_mut(&image_handle.handle) {
                    if frame_data.len() == image.data.len() {
                        image.data.copy_from_slice(&frame_data);
                        debug!("Texture updated");
                    } else {
                        eprintln!(
                            "Frame size mismatch: expected {} bytes, got {} bytes",
                            image.data.len(),
                            frame_data.len()
                        );
                    }
                }
            }
        }
    }
}
