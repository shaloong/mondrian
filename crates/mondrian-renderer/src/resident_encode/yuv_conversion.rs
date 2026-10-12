//! Shared resident RGB-to-YCbCr code values and spatial sampling.

use super::ResidentEncodeAdapterContract;
use mondrian_media::ResidentEncodeBitDepth;

pub(super) fn conversion_shader(
    contract: ResidentEncodeAdapterContract,
    row_pitch: u32,
    chroma_offset: u32,
) -> String {
    let (kr, kb) = match contract.colorimetry {
        mondrian_media::ResidentEncodeColorimetry::Rec709 => (0.2126_f32, 0.0722_f32),
        mondrian_media::ResidentEncodeColorimetry::Rec2100Pq
        | mondrian_media::ResidentEncodeColorimetry::Rec2100Hlg => (0.2627_f32, 0.0593_f32),
    };
    let (y_offset, y_scale, c_offset, c_scale, max_code, shift) =
        match (contract.bit_depth, contract.full_range) {
            (ResidentEncodeBitDepth::Eight, false) => (16.0, 219.0, 128.0, 224.0, 255.0, 0),
            (ResidentEncodeBitDepth::Eight, true) => (0.0, 255.0, 127.5, 255.0, 255.0, 0),
            (ResidentEncodeBitDepth::Ten, false) => (64.0, 876.0, 512.0, 896.0, 1023.0, 6),
            (ResidentEncodeBitDepth::Ten, true) => (0.0, 1023.0, 511.5, 1023.0, 1023.0, 6),
        };
    let header = format!(
        "@group(0) @binding(0) var source: texture_2d<f32>;\n\
         @group(0) @binding(1) var<storage, read_write> output: array<u32>;\n\
         const WIDTH: u32 = {width}u;\nconst HEIGHT: u32 = {height}u;\n\
         const ROW_WORDS: u32 = {row_words}u;\nconst CHROMA_WORD: u32 = {chroma_word}u;\n\
         const KR: f32 = {kr};\nconst KB: f32 = {kb};\nconst KG: f32 = 1.0 - KR - KB;\n\
         const Y_OFFSET: f32 = {y_offset};\nconst Y_SCALE: f32 = {y_scale};\n\
         const C_OFFSET: f32 = {c_offset};\nconst C_SCALE: f32 = {c_scale};\n\
         const MAX_CODE: f32 = {max_code};\n\
         fn rgb(x: u32, y: u32) -> vec3<f32> {{\n  return clamp(textureLoad(source, vec2<i32>(i32(min(x, WIDTH - 1u)), i32(min(y, HEIGHT - 1u))), 0).rgb, vec3<f32>(0.0), vec3<f32>(1.0));\n}}\n\
         fn yuv(value: vec3<f32>) -> vec3<f32> {{\n  let y = dot(value, vec3<f32>(KR, KG, KB));\n  let cb = (value.b - y) / (2.0 * (1.0 - KB));\n  let cr = (value.r - y) / (2.0 * (1.0 - KR));\n  return vec3<f32>(y, cb, cr);\n}}\n\
         fn quant_y(value: f32) -> u32 {{ return u32(round(clamp(Y_OFFSET + Y_SCALE * value, 0.0, MAX_CODE))); }}\n\
         fn quant_c(value: f32) -> u32 {{ return u32(round(clamp(C_OFFSET + C_SCALE * value, 0.0, MAX_CODE))); }}\n",
        width = contract.width,
        height = contract.height,
        row_words = row_pitch / 4,
        chroma_word = chroma_offset / 4,
    );
    let body = match (contract.bit_depth, contract.chroma_location) {
        (ResidentEncodeBitDepth::Eight, mondrian_media::ResidentEncodeChromaLocation::Center) => {
            r#"
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
  let x = gid.x * 4u;
  let y0 = gid.y * 2u;
  if (x >= WIDTH || y0 >= HEIGHT) { return; }
  let y1 = min(y0 + 1u, HEIGHT - 1u);
  let a0 = yuv(rgb(x, y0)); let b0 = yuv(rgb(x + 1u, y0));
  let c0 = yuv(rgb(x + 2u, y0)); let d0 = yuv(rgb(x + 3u, y0));
  let a1 = yuv(rgb(x, y1)); let b1 = yuv(rgb(x + 1u, y1));
  let c1 = yuv(rgb(x + 2u, y1)); let d1 = yuv(rgb(x + 3u, y1));
  output[y0 * ROW_WORDS + x / 4u] = quant_y(a0.x) | (quant_y(b0.x) << 8u) | (quant_y(c0.x) << 16u) | (quant_y(d0.x) << 24u);
  output[y1 * ROW_WORDS + x / 4u] = quant_y(a1.x) | (quant_y(b1.x) << 8u) | (quant_y(c1.x) << 16u) | (quant_y(d1.x) << 24u);
  let uv0 = (a0.yz + b0.yz + a1.yz + b1.yz) * 0.25;
  let uv1 = (c0.yz + d0.yz + c1.yz + d1.yz) * 0.25;
  output[CHROMA_WORD + (y0 / 2u) * ROW_WORDS + x / 4u] = quant_c(uv0.x) | (quant_c(uv0.y) << 8u) | (quant_c(uv1.x) << 16u) | (quant_c(uv1.y) << 24u);
}
"#
        }
        (ResidentEncodeBitDepth::Ten, mondrian_media::ResidentEncodeChromaLocation::Center) => {
            r#"
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
  let x = gid.x * 2u;
  let y0 = gid.y * 2u;
  if (x >= WIDTH || y0 >= HEIGHT) { return; }
  let y1 = min(y0 + 1u, HEIGHT - 1u);
  let a0 = yuv(rgb(x, y0)); let b0 = yuv(rgb(x + 1u, y0));
  let a1 = yuv(rgb(x, y1)); let b1 = yuv(rgb(x + 1u, y1));
  output[y0 * ROW_WORDS + x / 2u] = (quant_y(a0.x) << 6u) | (quant_y(b0.x) << 22u);
  output[y1 * ROW_WORDS + x / 2u] = (quant_y(a1.x) << 6u) | (quant_y(b1.x) << 22u);
  let uv = (a0.yz + b0.yz + a1.yz + b1.yz) * 0.25;
  output[CHROMA_WORD + (y0 / 2u) * ROW_WORDS + x / 2u] = (quant_c(uv.x) << 6u) | (quant_c(uv.y) << 22u);
}
"#
        }
        (ResidentEncodeBitDepth::Eight, mondrian_media::ResidentEncodeChromaLocation::Left) => {
            r#"
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
  let x = gid.x * 4u;
  let y0 = gid.y * 2u;
  if (x >= WIDTH || y0 >= HEIGHT) { return; }
  let y1 = min(y0 + 1u, HEIGHT - 1u);
  let left_x = max(x, 1u) - 1u;
  let p0 = yuv(rgb(left_x, y0)); let a0 = yuv(rgb(x, y0)); let b0 = yuv(rgb(x + 1u, y0));
  let c0 = yuv(rgb(x + 2u, y0)); let d0 = yuv(rgb(x + 3u, y0));
  let p1 = yuv(rgb(left_x, y1)); let a1 = yuv(rgb(x, y1)); let b1 = yuv(rgb(x + 1u, y1));
  let c1 = yuv(rgb(x + 2u, y1)); let d1 = yuv(rgb(x + 3u, y1));
  output[y0 * ROW_WORDS + x / 4u] = quant_y(a0.x) | (quant_y(b0.x) << 8u) | (quant_y(c0.x) << 16u) | (quant_y(d0.x) << 24u);
  output[y1 * ROW_WORDS + x / 4u] = quant_y(a1.x) | (quant_y(b1.x) << 8u) | (quant_y(c1.x) << 16u) | (quant_y(d1.x) << 24u);
  let uv0 = (p0.yz + 2.0 * a0.yz + b0.yz + p1.yz + 2.0 * a1.yz + b1.yz) * 0.125;
  let uv1 = (b0.yz + 2.0 * c0.yz + d0.yz + b1.yz + 2.0 * c1.yz + d1.yz) * 0.125;
  output[CHROMA_WORD + (y0 / 2u) * ROW_WORDS + x / 4u] = quant_c(uv0.x) | (quant_c(uv0.y) << 8u) | (quant_c(uv1.x) << 16u) | (quant_c(uv1.y) << 24u);
}
"#
        }
        (ResidentEncodeBitDepth::Ten, mondrian_media::ResidentEncodeChromaLocation::Left) => {
            r#"
@compute @workgroup_size(8, 8)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
  let x = gid.x * 2u;
  let y0 = gid.y * 2u;
  if (x >= WIDTH || y0 >= HEIGHT) { return; }
  let y1 = min(y0 + 1u, HEIGHT - 1u);
  let left_x = max(x, 1u) - 1u;
  let p0 = yuv(rgb(left_x, y0)); let a0 = yuv(rgb(x, y0)); let b0 = yuv(rgb(x + 1u, y0));
  let p1 = yuv(rgb(left_x, y1)); let a1 = yuv(rgb(x, y1)); let b1 = yuv(rgb(x + 1u, y1));
  output[y0 * ROW_WORDS + x / 2u] = (quant_y(a0.x) << 6u) | (quant_y(b0.x) << 22u);
  output[y1 * ROW_WORDS + x / 2u] = (quant_y(a1.x) << 6u) | (quant_y(b1.x) << 22u);
  let uv = (p0.yz + 2.0 * a0.yz + b0.yz + p1.yz + 2.0 * a1.yz + b1.yz) * 0.125;
  output[CHROMA_WORD + (y0 / 2u) * ROW_WORDS + x / 2u] = (quant_c(uv.x) << 6u) | (quant_c(uv.y) << 22u);
}
"#
        }
    };
    let _ = shift;
    header + body
}

#[cfg(test)]
mod tests {
    use super::*;
    use mondrian_media::ResidentEncodeColorimetry;

    fn contract(
        bit_depth: ResidentEncodeBitDepth,
        full_range: bool,
    ) -> ResidentEncodeAdapterContract {
        ResidentEncodeAdapterContract {
            width: 1920,
            height: 1080,
            frame_rate_num: 30_000,
            frame_rate_den: 1_001,
            bit_depth,
            colorimetry: ResidentEncodeColorimetry::Rec709,
            full_range,
            chroma_location: mondrian_media::ResidentEncodeChromaLocation::Left,
            max_frames_in_flight: 3,
        }
    }

    #[test]
    fn shaders_encode_exact_legal_and_full_range_constants() {
        let legal = conversion_shader(
            contract(ResidentEncodeBitDepth::Eight, false),
            2048,
            2048 * 1080,
        );
        assert!(legal.contains("const Y_OFFSET: f32 = 16"));
        assert!(legal.contains("const Y_SCALE: f32 = 219"));
        assert!(legal.contains("const C_SCALE: f32 = 224"));
        let full = conversion_shader(
            contract(ResidentEncodeBitDepth::Ten, true),
            4096,
            4096 * 1080,
        );
        assert!(full.contains("const Y_SCALE: f32 = 1023"));
        assert!(full.contains("const C_OFFSET: f32 = 511.5"));
        assert!(full.contains("<< 22u"));
        assert!(full.contains("let left_x = max(x, 1u) - 1u"));
        assert!(full.contains("2.0 * a0.yz"));
        assert!(full.contains("* 0.125"));

        let centered = conversion_shader(
            ResidentEncodeAdapterContract {
                chroma_location: mondrian_media::ResidentEncodeChromaLocation::Center,
                ..contract(ResidentEncodeBitDepth::Eight, false)
            },
            2048,
            2048 * 1080,
        );
        assert!(centered.contains("(a0.yz + b0.yz + a1.yz + b1.yz) * 0.25"));
    }
}
