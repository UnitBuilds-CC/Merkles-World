// A WASI preview1 host shim written once and used twice: under bare V8 (Node's WebAssembly API, no
// `node:wasi`) and inside a browser engine. Same shim, two engines - so if the printed roots disagree,
// the disagreement is about the engine and not about which host code ran.
//
// Why hand-write a shim at all. `node:wasi` proves the module runs under V8 in a CLI host, which is what
// CI's `roots executed in WASM` job measures. A browser has no WASI, so the only way to ask "do these
// roots survive a browser engine" is to provide the handful of host functions this module actually
// imports - 12 of them, no clocks and no randomness in the release build.
//
// Deliberately minimal, and the minimality is the claim: the boot writes to stdout and reads nothing, so
// `fd_read`, `path_open` and the prestat probes answer errors rather than pretending to have a
// filesystem. A shim that quietly succeeded at opening files would be a different program.
//
// Error numbers are wasi-snapshot-preview1's: EBADF 8, ENOENT 44.

const EBADF = 8;
const ENOENT = 44;

/**
 * @param {() => WebAssembly.Memory} memoryOf resolved lazily, because the shim is built before the
 *   instance exists and the instance is what owns the memory.
 * @param {{args?: string[]}} [options] the argv the module sees. Default is a bare command name, which
 *   is how the engine boots its self-contained scene.
 */
export function buildWasiShim(memoryOf, options = {}) {
  const args = options.args ?? ["engine"];
  let out = new Uint8Array(0);

  const bytes = () => new Uint8Array(memoryOf().buffer);
  const view32 = () => new Uint32Array(memoryOf().buffer);

  const append = (chunk) => {
    const next = new Uint8Array(out.length + chunk.length);
    next.set(out, 0);
    next.set(chunk, out.length);
    out = next;
  };

  /** Read one `wasi_iovec` (u32 base, u32 len) and copy the bytes it points at. */
  const iovecBytes = (ptr, count) => {
    const parts = [];
    const words = view32();
    for (let i = 0; i < count; i += 1) {
      const base = words[ptr / 4 + i * 2];
      const len = words[ptr / 4 + i * 2 + 1];
      parts.push(bytes().subarray(base, base + len));
    }
    return parts;
  };

  const writeStr = (ptr, text) => {
    const target = bytes();
    for (let i = 0; i < text.length; i += 1) {
      target[ptr + i] = text.charCodeAt(i);
    }
    target[ptr + text.length] = 0;
  };

  const shim = {
    args_sizes_get: (argcPtr, bufSizePtr) => {
      const total = args.reduce((sum, a) => sum + a.length + 1, 0);
      view32()[argcPtr / 4] = args.length;
      view32()[bufSizePtr / 4] = total;
      return 0;
    },
    args_get: (argvPtr, argvBufPtr) => {
      // wasi-libc asks for sizes first, then a buffer to fill; the pointers it hands over are its own,
      // so writing the strings here is the whole job.
      let cursor = argvBufPtr;
      const words = view32();
      for (let i = 0; i < args.length; i += 1) {
        words[argvPtr / 4 + i] = cursor;
        writeStr(cursor, args[i]);
        cursor += args[i].length + 1;
      }
      return 0;
    },
    environ_sizes_get: (countPtr, sizePtr) => {
      view32()[countPtr / 4] = 0;
      view32()[sizePtr / 4] = 0;
      return 0;
    },
    environ_get: () => 0,
    fd_write: (fd, iovPtr, iovCount, nwrittenPtr) => {
      if (fd !== 1 && fd !== 2) return EBADF;
      let written = 0;
      for (const part of iovecBytes(iovPtr, iovCount)) {
        append(part);
        written += part.length;
      }
      view32()[nwrittenPtr / 4] = written;
      return 0;
    },
    fd_read: () => EBADF,
    fd_close: (fd) => (fd <= 2 ? 0 : EBADF),
    fd_filestat_get: (fd, statPtr) => {
      // 64-byte `wasi_filestat`: dev, ino, filetype, nlink, size, atim, mtim, ctim.
      if (fd !== 1 && fd !== 2) return EBADF;
      const words = view32();
      for (let i = 0; i < 16; i += 1) words[statPtr / 4 + i] = 0;
      words[statPtr / 4 + 4] = 2; // __WASI_FILETYPE_CHARACTER_DEVICE
      return 0;
    },
    path_open: () => ENOENT,
    fd_prestat_get: () => EBADF,
    fd_prestat_dir_name: () => EBADF,
    proc_exit: (code) => {
      throw new Error(`proc_exit(${code})`);
    },
  };

  return {
    module: shim,
    /** Everything the boot printed, as text. The caller diffs this against a native run. */
    text: () => new TextDecoder().decode(out),
    bytes: () => out,
  };
}

/** The import object shape WebAssembly.instantiate expects for a wasip1 command. */
export function importObject(shim) {
  return { wasi_snapshot_preview1: shim.module };
}
