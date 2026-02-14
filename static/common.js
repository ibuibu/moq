// --- VarInt (RFC 9000 Section 16) ---
function encodeVarInt(value) {
  if (value < 0x40) {
    return new Uint8Array([value]);
  } else if (value < 0x4000) {
    const buf = new Uint8Array(2);
    new DataView(buf.buffer).setUint16(0, 0x4000 | value);
    return buf;
  } else if (value < 0x40000000) {
    const buf = new Uint8Array(4);
    new DataView(buf.buffer).setUint32(0, 0x80000000 | value);
    return buf;
  } else {
    const buf = new Uint8Array(8);
    const dv = new DataView(buf.buffer);
    // value を上位32bit / 下位32bit に分割
    const hi = Math.floor(value / 0x100000000);
    const lo = value >>> 0;
    dv.setUint32(0, 0xc0000000 | hi);
    dv.setUint32(4, lo);
    return buf;
  }
}

function decodeVarInt(buf, offset) {
  const first = buf[offset];
  const prefix = first >> 6;
  const len = 1 << prefix;
  if (offset + len > buf.length) throw new Error('not enough data for varint');

  let val = first & 0x3f;
  for (let i = 1; i < len; i++) {
    val = val * 256 + buf[offset + i];
  }
  return { value: val, bytesRead: len };
}

// --- String encode/decode ---
function encodeString(str) {
  const encoded = new TextEncoder().encode(str);
  const lenBytes = encodeVarInt(encoded.length);
  const result = new Uint8Array(lenBytes.length + encoded.length);
  result.set(lenBytes);
  result.set(encoded, lenBytes.length);
  return result;
}

function decodeString(buf, offset) {
  const { value: len, bytesRead } = decodeVarInt(buf, offset);
  offset += bytesRead;
  if (offset + len > buf.length) throw new Error('not enough data for string');
  const str = new TextDecoder().decode(buf.slice(offset, offset + len));
  return { value: str, bytesRead: bytesRead + len };
}

function concatBytes(arrays) {
  const totalLen = arrays.reduce((sum, a) => sum + a.length, 0);
  const result = new Uint8Array(totalLen);
  let offset = 0;
  for (const a of arrays) {
    result.set(a, offset);
    offset += a.length;
  }
  return result;
}

// --- Uint16 encode ---
function encodeUint16(value) {
  const buf = new Uint8Array(2);
  new DataView(buf.buffer).setUint16(0, value);
  return buf;
}

// --- MoQ Messages ---
function encodeClientSetup() {
  const msgType = encodeVarInt(0x20); // CLIENT_SETUP (draft-15)
  // Payload: num_params(varint=0)
  const payload = encodeVarInt(0);
  const length = encodeUint16(payload.length);
  return concatBytes([msgType, length, payload]);
}

// decodeMessage は各ページ側で定義（viewer/publisher で異なるメッセージを扱う）
async function readMessage(reader, existingBuf) {
  let buf = existingBuf || new Uint8Array(0);

  while (true) {
    try {
      const msg = decodeMessage(buf);
      const remaining = buf.slice(msg.totalBytes);
      return { msg, remaining };
    } catch (e) {
      // need more data
    }
    const { done, value } = await reader.read();
    if (done) throw new Error('stream closed while reading message');
    const newBuf = new Uint8Array(buf.length + value.length);
    newBuf.set(buf);
    newBuf.set(value, buf.length);
    buf = newBuf;
  }
}
