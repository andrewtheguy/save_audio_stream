// EBML/WebM writing helpers

pub fn write_ebml_id(buf: &mut Vec<u8>, id: u32) {
    // EBML IDs already include their size marker bits, just write raw bytes
    if id <= 0xFF {
        buf.push(id as u8);
    } else if id <= 0xFFFF {
        buf.push((id >> 8) as u8);
        buf.push(id as u8);
    } else if id <= 0xFFFFFF {
        buf.push((id >> 16) as u8);
        buf.push((id >> 8) as u8);
        buf.push(id as u8);
    } else {
        buf.push((id >> 24) as u8);
        buf.push((id >> 16) as u8);
        buf.push((id >> 8) as u8);
        buf.push(id as u8);
    }
}

pub fn write_ebml_size(buf: &mut Vec<u8>, size: u64) {
    if size <= 0x7E {
        buf.push((size | 0x80) as u8);
    } else if size <= 0x3FFE {
        buf.push(((size >> 8) | 0x40) as u8);
        buf.push(size as u8);
    } else if size <= 0x1FFFFE {
        buf.push(((size >> 16) | 0x20) as u8);
        buf.push((size >> 8) as u8);
        buf.push(size as u8);
    } else if size <= 0x0FFFFFFE {
        buf.push(((size >> 24) | 0x10) as u8);
        buf.push((size >> 16) as u8);
        buf.push((size >> 8) as u8);
        buf.push(size as u8);
    } else {
        // 8-byte size for unknown/streaming
        buf.extend_from_slice(&[0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
    }
}

pub fn write_ebml_uint(buf: &mut Vec<u8>, id: u32, value: u64) {
    write_ebml_id(buf, id);
    let bytes = if value == 0 {
        1
    } else {
        ((64 - value.leading_zeros()) + 7) / 8
    } as usize;
    write_ebml_size(buf, bytes as u64);
    for i in (0..bytes).rev() {
        buf.push((value >> (i * 8)) as u8);
    }
}

pub fn write_ebml_string(buf: &mut Vec<u8>, id: u32, value: &str) {
    write_ebml_id(buf, id);
    write_ebml_size(buf, value.len() as u64);
    buf.extend_from_slice(value.as_bytes());
}

pub fn write_ebml_binary(buf: &mut Vec<u8>, id: u32, data: &[u8]) {
    write_ebml_id(buf, id);
    write_ebml_size(buf, data.len() as u64);
    buf.extend_from_slice(data);
}

pub fn write_ebml_float(buf: &mut Vec<u8>, id: u32, value: f64) {
    write_ebml_id(buf, id);
    write_ebml_size(buf, 8);
    buf.extend_from_slice(&value.to_be_bytes());
}
