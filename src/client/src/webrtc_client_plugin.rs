use bevy::image::ImageSampler;
use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use crossbeam_channel::{Receiver, Sender};
use futures_util::stream::StreamExt as StreamTraitExt;
use futures_util::sink::SinkExt as SinkTraitExt;
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app::AppSink;
use std::sync::Arc;
use tokio_tungstenite::connect_async;
use webrtc::api::APIBuilder;
use webrtc::api::media_engine::MediaEngine;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;
use webrtc::rtp_transceiver::RTCRtpTransceiver;
use webrtc::rtp_transceiver::rtp_receiver::RTCRtpReceiver;
use webrtc::track::track_remote::TrackRemote;

#[derive(Resource)]
pub struct VideoImageHandle(pub Handle<Image>);

#[derive(Resource)]
pub struct FrameReceiver(pub Receiver<Vec<u8>>);

#[derive(Resource)]
pub struct FrameSender(pub Sender<Vec<u8>>);

pub struct WebRTCClientPlugin;

impl Plugin for WebRTCClientPlugin {
    fn build(&self, app: &mut App) {
        gst::init().unwrap();

        let (tx, rx) = crossbeam_channel::unbounded();
        app.insert_resource(VideoImageHandle(Handle::default()))
            .insert_resource(FrameSender(tx))
            .insert_resource(FrameReceiver(rx))
            .add_systems(Startup, setup_video_ui)
            .add_systems(Startup, start_webrtc_client)
            .add_systems(Update, update_texture_from_frame);
    }
}

fn setup_video_ui(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let mut image = Image::new_fill(
        Extent3d {
            width: 640,
            height: 480,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        &[0, 0, 0, 255],
        TextureFormat::Rgba8UnormSrgb,
        Default::default(),
    );
    image.sampler = ImageSampler::nearest();

    let handle = images.add(image);

    commands.spawn(Camera2d::default());
    commands.spawn(Sprite {
        image: handle.clone(),
        ..default()
    });
    commands.insert_resource(VideoImageHandle(handle));
}

fn start_webrtc_client(tx: Res<FrameSender>) {
    let sender = tx.0.clone();

    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let mut me = MediaEngine::default();
            me.register_default_codecs().unwrap();
            let api = APIBuilder::new().with_media_engine(me).build();

            let (ws_stream, _) = connect_async("ws://localhost:3536").await.unwrap();
            let (mut ws_write, mut ws_read) = StreamTraitExt::split(ws_stream);

            let config = RTCConfiguration {
                ice_servers: vec![RTCIceServer {
                    urls: vec!["stun:stun.l.google.com:19302".into()],
                    ..Default::default()
                }],
                ..Default::default()
            };

            let pc = Arc::new(api.new_peer_connection(config).await.unwrap());

            let tx_clone = sender.clone();
            let pipeline = gst::Pipeline::new();

            // Create decoder pipeline
            let rtpdepay = gst::ElementFactory::make_with_name("rtpvp8depay", None).unwrap();
            let decoder = gst::ElementFactory::make_with_name("vp8dec", None).unwrap();
            let convert = gst::ElementFactory::make_with_name("videoconvert", None).unwrap();
            let videoscale = gst::ElementFactory::make_with_name("videoscale", None).unwrap();
            let sink = gst::ElementFactory::make_with_name("appsink", Some("mysink")).unwrap();
            let webrtcbin = gst::ElementFactory::make_with_name("webrtcbin", Some("recvbin")).unwrap();

            // Configure appsink
            let appsink = sink.clone().dynamic_cast::<AppSink>().unwrap();
            appsink.set_property("emit-signals", &true);
            appsink.set_property("sync", &false);
            appsink.set_caps(Some(
                &gst::Caps::builder("video/x-raw")
                    .field("format", &"RGBA")
                    .field("width", &640i32)
                    .field("height", &480i32)
                    .build(),
            ));

            let tx_for_sink = tx_clone.clone();
            let appsink_clone = appsink.clone();

            std::thread::spawn(move || {
                loop {
                    match appsink_clone.pull_sample() {
                        Ok(sample) => {
                            if let Some(buffer) = sample.buffer() {
                                if let Ok(map) = buffer.map_readable() {
                                    let frame = map.as_slice().to_vec();
                                    if tx_for_sink.send(frame).is_err() {
                                        warn!("❌ Failed to send frame to Bevy");
                                        break;
                                    }
                                }
                            }
                        }
                        Err(_) => {
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                    }
                }
            });
            pipeline
                .add_many(&[&rtpdepay, &decoder, &convert, &videoscale, &sink])
                .unwrap();
            gst::Element::link_many(&[&rtpdepay, &decoder, &convert, &sink]).unwrap();
            pipeline.add(&webrtcbin).unwrap();

            let rtpdepay_weak = rtpdepay.downgrade();
            webrtcbin.connect_pad_added(move |_webrtc, pad| {
                if pad.name().starts_with("video_") {
                    if let Some(rtpdepay) = rtpdepay_weak.upgrade() {
                        let sink_pad = rtpdepay.static_pad("sink").unwrap();
                        if !sink_pad.is_linked() {
                            if pad.link(&sink_pad).is_ok() {
                                info!("✅ Linked webrtcbin pad to rtpdepay");
                            } else {
                                warn!("❌ Failed to link webrtcbin pad");
                            }
                        }
                    }
                }
            });

            pipeline.set_state(gst::State::Playing).unwrap();

            pc.on_track(Box::new(
                |_track: Arc<TrackRemote>,
                 _receiver: Arc<RTCRtpReceiver>,
                 _transceiver: Arc<RTCRtpTransceiver>| {
                    info!("📡 WebRTC track received");
                    Box::pin(async {})
                },
            ));

            // WebSocket signaling
            while let Some(Ok(msg)) = ws_read.next().await {
                if let tokio_tungstenite::tungstenite::Message::Text(text) = msg {
                    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
                    if v["type"] == "offer" {
                        let offer = RTCSessionDescription::offer(v["sdp"].as_str().unwrap().into());
                        pc.set_remote_description(offer.unwrap()).await.unwrap();
                        let answer = pc.create_answer(None).await.unwrap();
                        pc.set_local_description(answer.clone()).await.unwrap();
                        let msg = serde_json::json!({ "type": "answer", "sdp": answer.sdp });
                        ws_write
                            .send(tokio_tungstenite::tungstenite::Message::Text(
                                msg.to_string().into(),
                            ))
                            .await
                            .unwrap();
                    } else if v["type"] == "candidate" {
                        if let Some(candidate) = v["candidate"].as_str() {
                            pc.add_ice_candidate(
                                webrtc::ice_transport::ice_candidate::RTCIceCandidateInit {
                                    candidate: candidate.to_string(),
                                    sdp_mid: v["sdpMid"].as_str().map(|s| s.to_string()),
                                    sdp_mline_index: v["sdpMLineIndex"].as_u64().map(|x| x as u16),
                                    username_fragment: None,
                                },
                            )
                                .await
                                .unwrap();
                        }
                    }
                }
            }
        });
    });
}

fn update_texture_from_frame(
    frame_rx: Res<FrameReceiver>,
    handle: Res<VideoImageHandle>,
    mut images: ResMut<Assets<Image>>,
) {
    while let Ok(frame) = frame_rx.0.try_recv() {
        if let Some(image) = images.get_mut(&handle.0) {
            let expected_size =
                image.texture_descriptor.size.width * image.texture_descriptor.size.height * 3; // RGB format

            if frame.len() as u32 != expected_size {
                warn!(
                    "⚠️ Frame size mismatch: expected {}, got {}",
                    expected_size,
                    frame.len()
                );
                continue;
            }

            image.data.copy_from_slice(&frame);
        }
    }
}
