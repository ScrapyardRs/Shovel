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

pub const fn create_sorted_coordinates<const RADIUS: i32>(
) -> [(i32, i32); ((RADIUS as usize * 2) + 1) * ((RADIUS as usize * 2) + 1)] {
    let mut coords = [(0, 0); ((RADIUS as usize * 2) + 1) * ((RADIUS as usize * 2) + 1)];
    let mut distances = [0; ((RADIUS as usize * 2) + 1) * ((RADIUS as usize * 2) + 1)];
    let mut i = 0;
    let mut x = -RADIUS;
    let mut z = -RADIUS;
    while i < ((RADIUS as usize * 2) + 1) * ((RADIUS as usize * 2) + 1) {
        if i != 0 && i % ((RADIUS as usize * 2) + 1) == 0 {
            x += 1;
            z = -RADIUS;
        }
        coords[i as usize] = (x, z);
        distances[i as usize] = (x * x) + (z * z);
        z += 1;
        i = i + 1;
    }

    let mut swapped = true;
    while swapped {
        swapped = false;
        let mut i = 0;
        while i < (((RADIUS as usize * 2) + 1) * ((RADIUS as usize * 2) + 1)) - 1 {
            if distances[i + 1] < distances[i] {
                distances.swap(i, i + 1);
                coords.swap(i, i + 1);
                swapped = true;
            }
            i = i + 1;
        }
    }

    coords
}
