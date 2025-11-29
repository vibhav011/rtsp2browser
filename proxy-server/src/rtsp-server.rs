use glib;
use gstreamer as gst;
use gstreamer_rtsp_server as gst_rtsp_server;
use gst_rtsp_server::prelude::*;

fn main() {
    gst::init().expect("Failed to initialize GStreamer.");

    let main_loop = glib::MainLoop::new(None, false);
    let server = gst_rtsp_server::RTSPServer::new();

    // Configure server address and port if needed
    // server.set_address("127.0.0.1");
    // server.set_service("8554");

    let mounts = server.mount_points().unwrap();

    // Create a media factory
    let factory = gst_rtsp_server::RTSPMediaFactory::new();
    factory.set_launch("( videotestsrc ! \
        x264enc tune=zerolatency \
        speed-preset=veryfast \
        key-int-max=0 \
        byte-stream=true \
        aud=true \
        sliced-threads=true \
        intra-refresh=false ! \
        video/x-h264,profile=baseline ! \
        rtph264pay config-interval=0 mtu=1000 auto-header-extension=false \
        timestamp-offset=0 name=pay0 pt=96 )");
    factory.set_shared(true); // Allow multiple clients to connect

    // Add the factory to a mount point
    mounts.add_factory("/test", factory);

    // Attach the server to the GLib main loop context
    server.attach(None).expect("Cannot attach server to context.");

    println!("RTSP stream ready at rtsp://127.0.0.1:8554/test");
    main_loop.run();
}
