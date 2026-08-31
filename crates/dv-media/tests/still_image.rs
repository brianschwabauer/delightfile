//! Still images as timeline clips (§1, M7): the engine decodes PNG/JPG/WebP
//! through the SAME VideoDecoder path as video — ffmpeg's image2 demuxer
//! exposes a one-frame video stream, and the playback/export "hold last
//! frame to segment end" rule turns that into a still clip. This test pins
//! the assumption.

use dv_media::VideoDecoder;

#[test]
fn png_decodes_as_one_frame_video() {
    let dir = tempfile::tempdir().expect("tempdir");
    let png = dir.path().join("still.png");
    // 64×36 gradient — enough pixels to be a real decode.
    let img = image::RgbImage::from_fn(64, 36, |x, y| image::Rgb([x as u8 * 4, y as u8 * 7, 128]));
    img.save(&png).expect("write png");

    let mut dec = VideoDecoder::open(&png).expect("VideoDecoder opens a PNG");
    let frame = dec.next_frame().expect("decode").expect("one frame exists");
    assert_eq!((frame.width, frame.height), (64, 36));

    // EOF after the single frame — the segment-hold rule covers the rest.
    assert!(dec.next_frame().expect("eof read").is_none());

    // A seek back to 0 must land on the frame again (scrubbing over stills).
    dec.seek(0).expect("seek 0");
    let again = dec.next_frame().expect("decode").expect("frame after seek");
    assert_eq!((again.width, again.height), (64, 36));
}
