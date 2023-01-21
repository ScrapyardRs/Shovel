use mcprotocol::common::play::SimpleLocation;

pub fn wrap_degrees(f: f32) -> f32 {
    let mut f = f;
    while f >= 180.0 {
        f -= 360.0;
    }
    while f < -180.0 {
        f += 360.0;
    }
    f
}

fn encode_v(v: f64) -> i64 {
    (v * 4096.0).round() as i64
}

pub fn encode_position(from: SimpleLocation, to: SimpleLocation) -> (i64, i64, i64) {
    (
        encode_v(to.x) - encode_v(from.x),
        encode_v(to.y) - encode_v(from.y),
        encode_v(to.z) - encode_v(from.z),
    )
}
