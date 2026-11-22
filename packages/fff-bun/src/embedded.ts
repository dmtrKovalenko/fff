/**
 * Tools for standalone-executable native library embedding.
 *
 * `bun build --compile` only bundles files referenced by statically analyzable
 * imports. Runtime resolution (./download.ts) is invisible to the bundler, so
 * we additionally reference the platform's native lib through a `type: "file"`
 * import. Bun then embeds it and returns a `$bunfs` path inside the compiled
 * binary (and the real on-disk path under `bun run`).
 *
 * Linux libc cannot be detected at build time, so it is supplied via the
 * FFF_LIBC build constant (`bun build --define FFF_LIBC='"musl"'`), defaulting
 * to glibc. macOS/Windows need no define.
 */

async function importFile(promise: Promise<{ default: string }>): Promise<string | null> {
  try {
    return (await promise).default;
  } catch {
    return null;
  }
}

async function resolveEmbeddedLibPath(): Promise<string | null> {
  if (process.platform === "darwin") {
    return importFile(
      import(`@ff-labs/fff-bin-darwin-${process.arch}/libfff_c.dylib`, {
        with: { type: "file" },
      }),
    );
  }

  if (process.platform === "win32") {
    return importFile(
      import(`@ff-labs/fff-bin-win32-${process.arch}/fff_c.dll`, {
        with: { type: "file" },
      }),
    );
  }

  if (process.platform === "freebsd") {
    return importFile(
      import(`@ff-labs/fff-bin-freebsd-${process.arch}/libfff_c.so`, {
        with: { type: "file" },
      }),
    );
  }

  if (process.platform === "linux") {
    return importFile(
      import(
        `@ff-labs/fff-bin-linux-${process.arch}-${typeof FFF_LIBC === "string" ? FFF_LIBC : "gnu"}/libfff_c.so`,
        { with: { type: "file" } }
      ),
    );
  }

  return null;
}

// Resolved once at module init so loadLibrary() can stay synchronous.
export const embeddedLibPath: string | null = await resolveEmbeddedLibPath();
