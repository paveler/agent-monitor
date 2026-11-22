// Tray icon drawn at runtime: a ring when nothing waits, otherwise the count on red (blocked) or blue.
const SIZE: usize = 32;

const DIGITS: [[u8; 7]; 10] = [
    [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
    [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
    [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
    [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110],
    [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
    [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
    [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
    [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
    [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
    [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
];

fn put(buf: &mut [u8], x: usize, y: usize, rgb: [u8; 3], a: f32) {
    if x >= SIZE || y >= SIZE || a <= 0.0 {
        return;
    }
    let i = (y * SIZE + x) * 4;
    let a = a.min(1.0);
    let dst_a = buf[i + 3] as f32 / 255.0;
    let out_a = a + dst_a * (1.0 - a);
    for c in 0..3 {
        let v = (rgb[c] as f32 * a + buf[i + c] as f32 * dst_a * (1.0 - a)) / out_a.max(0.001);
        buf[i + c] = v as u8;
    }
    buf[i + 3] = (out_a * 255.0) as u8;
}

fn digit(buf: &mut [u8], d: usize, x0: usize, y0: usize, scale: usize) {
    for (row, bits) in DIGITS[d].iter().enumerate() {
        for col in 0..5 {
            if bits & (1 << (4 - col)) != 0 {
                for dy in 0..scale {
                    for dx in 0..scale {
                        put(buf, x0 + col * scale + dx, y0 + row * scale + dy, [255, 255, 255], 1.0);
                    }
                }
            }
        }
    }
}

/// 32×32 RGBA.
pub fn render(count: usize, blocking: bool) -> Vec<u8> {
    let mut buf = vec![0u8; SIZE * SIZE * 4];
    let bg = if count == 0 { [43, 43, 46] } else if blocking { [255, 84, 77] } else { [72, 133, 237] };
    // Rounded square.
    let r = 8.0f32;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let cx = fx.clamp(r, SIZE as f32 - r);
            let cy = fy.clamp(r, SIZE as f32 - r);
            let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt();
            put(&mut buf, x, y, bg, r - d + 0.5);
        }
    }
    if count == 0 {
        // A usage ring, three quarters full.
        for y in 0..SIZE {
            for x in 0..SIZE {
                let (dx, dy) = (x as f32 + 0.5 - 16.0, y as f32 + 0.5 - 16.0);
                let d = (dx * dx + dy * dy).sqrt();
                let a = (10.5 - d + 0.5).min(d - 6.5 + 0.5);
                let filled = !(dx < 0.0 && dy < 0.0);
                put(&mut buf, x, y, if filled { [217, 120, 87] } else { [110, 110, 115] }, a);
            }
        }
    } else if count < 10 {
        digit(&mut buf, count, 9, 6, 3);
    } else {
        let n = count.min(99);
        digit(&mut buf, n / 10, 5, 9, 2);
        digit(&mut buf, n % 10, 17, 9, 2);
    }
    buf
}
