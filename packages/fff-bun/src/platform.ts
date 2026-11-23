/**
 * Platform detection utilities for downloading the correct binary
 */

import { execSync } from "node:child_process";

/**
 * Get the platform triple (e.g., "x86_64-unknown-linux-gnu")
 */
export function getTriple(): string {
  const platform = process.platform;
  const arch = process.arch;

  let osName: string;
  if (platform === "darwin") {
    osName = "apple-darwin";
  } else if (platform === "android") {
    osName = "linux-android";
  } else if (platform === "linux") {
    osName = detectLinuxLibc();
  } else if (platform === "win32") {
    osName = "pc-windows-msvc";
  } else if (platform === "freebsd") {
    osName = "unknown-freebsd";
  } else {
    throw new Error(`Unsupported platform: ${platform}`);
  }

  const archName = normalizeArch(arch);
  return `${archName}-${osName}`;
}

/**
 * Detect whether we're on musl or glibc Linux
 */
function detectLinuxLibc(): string {
  let output = "";
  try {
    output = execSync("ldd --version 2>&1", {
      encoding: "utf-8",
      timeout: 5000,
    });
  } catch (e: unknown) {
    // Alpine/musl: `ldd --version` exits with code 1 but still prints
    // "musl libc ..." — execSync surfaces that on the error object.
    const err = e as { stdout?: string | Buffer; stderr?: string | Buffer };
    output = String(err?.stdout ?? "") + String(err?.stderr ?? "");
  }
  if (output.toLowerCase().includes("musl")) {
    return "unknown-linux-musl";
  }
  return "unknown-linux-gnu";
}

/**
 * Normalize architecture name to Rust target format
 */
function normalizeArch(arch: string): string {
  switch (arch) {
    case "x64":
    case "amd64":
      return "x86_64";
    case "arm64":
      return "aarch64";
    case "arm":
      return "arm";
    default:
      throw new Error(`Unsupported architecture: ${arch}`);
  }
}

/**
 * Get the library file extension for the current platform
 */
export function getLibExtension(): "dylib" | "so" | "dll" {
  switch (process.platform) {
    case "darwin":
      return "dylib";
    case "win32":
      return "dll";
    default:
      return "so";
  }
}

/**
 * Get the library filename prefix (empty on Windows)
 */
export function getLibPrefix(): string {
  return process.platform === "win32" ? "" : "lib";
}

/**
 * Get the full library filename for the current platform
 */
export function getLibFilename(): string {
  const prefix = getLibPrefix();
  const ext = getLibExtension();
  return `${prefix}fff_c.${ext}`;
}

/**
 * Map from Rust target triple to npm platform package name
 */
const TRIPLE_TO_NPM_PACKAGE: Record<string, string> = {
  "aarch64-apple-darwin": "@ff-labs/fff-bin-darwin-arm64",
  "x86_64-apple-darwin": "@ff-labs/fff-bin-darwin-x64",
  "x86_64-unknown-linux-gnu": "@ff-labs/fff-bin-linux-x64-gnu",
  "aarch64-unknown-linux-gnu": "@ff-labs/fff-bin-linux-arm64-gnu",
  "x86_64-unknown-linux-musl": "@ff-labs/fff-bin-linux-x64-musl",
  "aarch64-unknown-linux-musl": "@ff-labs/fff-bin-linux-arm64-musl",
  "x86_64-pc-windows-msvc": "@ff-labs/fff-bin-win32-x64",
  "aarch64-pc-windows-msvc": "@ff-labs/fff-bin-win32-arm64",
  "aarch64-linux-android": "@ff-labs/fff-bin-android-arm64",
  "x86_64-unknown-freebsd": "@ff-labs/fff-bin-freebsd-x64",
  "aarch64-unknown-freebsd": "@ff-labs/fff-bin-freebsd-arm64",
};

/**
 * Get the npm package name for the current platform's native binary.
 *
 * @returns Package name like "@ff-labs/fff-bin-darwin-arm64"
 * @throws If the current platform is not supported
 */
export function getNpmPackageName(): string {
  const triple = getTriple();
  const packageName = TRIPLE_TO_NPM_PACKAGE[triple];
  if (!packageName) {
    throw new Error(`No npm package available for platform: ${triple}`);
  }
  return packageName;
}
