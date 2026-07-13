# Media test fixtures

`h264-bframes.mp4` is a synthetic 64×64, 25 fps H.264 stream containing B-frames. It is used to
exercise FFmpeg's real send/receive backpressure and decoder-reordering behavior across sequential
preview requests. The clip contains only generated `testsrc2` pixels and no third-party media.

It can be regenerated with:

```sh
ffmpeg -y -f lavfi -i "testsrc2=size=64x64:rate=25:duration=1.2" \
  -c:v libx264 -preset veryfast -g 30 -bf 3 -pix_fmt yuv420p \
  -colorspace bt709 -color_primaries bt709 -color_trc bt709 -color_range tv \
  -movflags +faststart -an h264-bframes.mp4
```
