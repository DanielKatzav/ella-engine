use bevy::prelude::*;
use crossbeam_channel::Sender;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::thread;
use tokio::net::TcpListener;
use tokio::runtime::Runtime;
use tokio::sync::Mutex;
use tokio_tungstenite::accept_async;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::APIBuilder;
use webrtc::ice_transport::ice_server::RTCIceServer;
use webrtc::media::Sample;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::RTCPeerConnection;
use webrtc::rtp_transceiver::rtp_codec::{
    RTCRtpCodecCapability, RTCRtpCodecParameters, RTPCodecType,
};
use webrtc::track::track_local::track_local_static_sample::TrackLocalStaticSample;
use webrtc::track::track_local::{TrackLocal, TrackLocalWriter};

use crate::gstreamer_plugin::GstReceiver;

#[derive(Resource)]
pub struct NewClientReceiver {
    pub receiver: crossbeam_channel::Receiver<Sender<Vec<u8>>>,
}

#[derive(Resource, Default)]
pub struct WebRTCConnections {
    pub senders: Vec<Sender<Vec<u8>>>,
}

pub struct WebRTCServerPlugin;

impl Plugin for WebRTCServerPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(WebRTCConnections::default())
            .add_systems(Startup, setup_webrtc_server)
            .add_systems(Update, accept_new_clients)
            .add_systems(Update, broadcast_vp8_frames);
    }
}

fn setup_webrtc_server(mut commands: Commands) {
    let (client_tx, client_rx) = crossbeam_channel::unbounded();
    commands.insert_resource(NewClientReceiver {
        receiver: client_rx,
    });

    thread::spawn(move || {
        let rt = Runtime::new().unwrap();
        rt.block_on(async move {
            let mut me = MediaEngine::default();
            me.register_codec(
                RTCRtpCodecParameters {
                    capability: RTCRtpCodecCapability {
                        mime_type: "video/VP8".to_owned(),
                        clock_rate: 90000,
                        ..Default::default()
                    },
                    payload_type: 96,
                    ..Default::default()
                },
                RTPCodecType::Video,
            ).unwrap();

            let api = APIBuilder::new().with_media_engine(me).build();

            let listener = TcpListener::bind("0.0.0.0:3536").await.unwrap();
            while let Ok((stream, _)) = listener.accept().await {
                let ws = accept_async(stream).await.unwrap();
                let (write, mut read) = ws.split();
                let write = Arc::new(Mutex::new(write));
                let write_for_ice = write.clone();

                let config = RTCConfiguration {
                    ice_servers: vec![RTCIceServer {
                        urls: vec!["stun:stun.l.google.com:19302".into()],
                        ..Default::default()
                    }],
                    ..Default::default()
                };
                let pc = Arc::new(api.new_peer_connection(config).await.unwrap());
                let pc_for_ice = pc.clone();

                let track = Arc::new(TrackLocalStaticSample::new(
                    RTCRtpCodecCapability {
                        mime_type: "video/VP8".into(),
                        clock_rate: 90000,
                        ..Default::default()
                    },
                    "video".into(),
                    "webrtc-rs".into(),
                ));

                pc.add_track(track.clone() as Arc<dyn TrackLocal + Send + Sync>)
                    .await
                    .unwrap();

                pc_for_ice.on_ice_candidate(Box::new(move |candidate| {
                    if let Some(c) = candidate {
                        let json = c.to_json().unwrap();
                        let msg = serde_json::json!({
                            "type": "candidate",
                            "candidate": json.candidate,
                            "sdpMid": json.sdp_mid,
                            "sdpMLineIndex": json.sdp_mline_index,
                        });
                        let value = write_for_ice.clone();
                        tokio::spawn(async move {
                            let mut sink = value.lock().await;
                            let _ = sink.send(tungstenite::Message::Text(msg.to_string().into())).await;
                        });
                    }
                    Box::pin(async {})
                }));

                let (vp8_tx, vp8_rx) = crossbeam_channel::unbounded::<Vec<u8>>();
                client_tx.send(vp8_tx).unwrap();

                let track_clone = track.clone();
                tokio::spawn(async move {
                    while let Ok(buf) = vp8_rx.recv() {
                        let sample = Sample {
                            data: buf.into(),
                            duration: std::time::Duration::from_millis(33),
                            ..Default::default()
                        };
                        debug!("📤 Sample sent");

                        if let Err(err) = track_clone.write_sample(&sample).await {
                            error!("Failed to write VP8 sample: {:?}", err);
                        }
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(33)).await;
                });

                let pc_clone = pc.clone();
                let write_clone = write.clone();
                tokio::spawn(async move {
                    let offer = pc_clone.create_offer(None).await.unwrap();
                    pc_clone.set_local_description(offer.clone()).await.unwrap();

                    let msg = serde_json::json!({ "type": "offer", "sdp": offer.sdp });
                    {
                        let mut ws_sink = write_clone.lock().await;
                        ws_sink.send(tungstenite::Message::Text(msg.to_string().into())).await.unwrap();
                    }

                    while let Some(Ok(msg)) = read.next().await {
                        if let tungstenite::Message::Text(txt) = msg {
                            let v: serde_json::Value = serde_json::from_str(&txt).unwrap();
                            if v["type"] == "answer" {
                                let ans = webrtc::peer_connection::sdp::session_description::RTCSessionDescription::answer(
                                    v["sdp"].as_str().unwrap().into(),
                                );
                                pc_clone.set_remote_description(ans.expect("REASON")).await.unwrap();
                                break;
                            }
                        }
                    }
                });
            }
            futures::future::pending::<()>().await;
        });
    });
}

fn accept_new_clients(
    mut connections: ResMut<WebRTCConnections>,
    new_clients: Res<NewClientReceiver>,
) {
    while let Ok(tx) = new_clients.receiver.try_recv() {
        info!("New client arrived");
        connections.senders.push(tx);
    }
}

fn broadcast_vp8_frames(gst: Res<GstReceiver>, connections: Res<WebRTCConnections>) {
    while let Ok(buf) = gst.receiver.try_recv() {
        for tx in &connections.senders {
            let _ = tx.send(buf.clone());
        }
    }
}
