/**
 * Binary resolution utilities for fff-node
 *
 * Resolves the native library from:
 * 1. The FFF_C_LIB environment variable (explicit path to libfff_c.so/.dylib/.dll)
 * 2. Platform-specific npm package (e.g. @ff-labs/fff-bin-darwin-arm64)
 * 3. Local dev build (target/release or target/debug)
 */

import { existsSync, readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { getLibFilename, getNpmPackageName } from "./platform.js";

/**
 * Get the current file's directory
 */
function getCurrentDir(): string {
  // CJS build: import.meta.url is inlined at bundle time, __dirname is the truth
  if (typeof __dirname !== "undefined") return __dirname;

  const url = import.meta.url;

  if (url.startsWith("file://")) {
    return dirname(fileURLToPath(url));
  }
  return dirname(url);
}

/**
 * Get the package root directory
 */
function getPackageDir(): string {
  const currentDir = getCurrentDir();
  // In dev: src/ -> package root
  // In dist: dist/ -> package root
  // We look for package.json to find the actual root
  let dir = currentDir;
  for (let i = 0; i < 5; i++) {
    if (existsSync(join(dir, "package.json"))) {
      try {
        const pkg = JSON.parse(readFileSync(join(dir, "package.json"), "utf-8"));
        if (pkg.name === "@ff-labs/fff-node") {
          return dir;
        }
      } catch {
        // Not our package.json, keep going up
      }
    }
    dir = dirname(dir);
  }
  // Fallback: assume we're one level deep in src/
  return dirname(currentDir);
}

/**
 * Check if the binary exists in any known location
 */
export function binaryExists(): boolean {
  return findBinary() !== null;
}

/**
 * Try to resolve the binary from the platform-specific npm package.
 *
 * When users install @ff-labs/fff-node, npm automatically installs the matching
 * optionalDependency (e.g. @ff-labs/fff-bin-darwin-arm64). We resolve the binary
 * path by requiring that package's package.json and looking for the binary
 * in the same directory.
 */
function resolveFromNpmPackage(): string | null {
  let packageName: string;
  try {
    packageName = getNpmPackageName();
  } catch {
    // Unsupported platform, or no platform package mapped for it.
    return null;
  }

  try {
    // Use createRequire to resolve the platform package's location
    const require = createRequire(join(getPackageDir(), "package.json"));
    const packageJsonPath = require.resolve(`${packageName}/package.json`);
    const packageDir = dirname(packageJsonPath);
    const binaryPath = join(packageDir, getLibFilename());

    if (existsSync(binaryPath)) {
      return binaryPath;
    }
  } catch {
    // Package not installed - this is expected on unsupported platforms
    // or when installed without optional dependencies
  }

  return null;
}

/**
 * Get the development binary path (for local development)
 */
function getDevBinaryPath(): string | null {
  const packageDir = getPackageDir();
  const workspaceRoot = join(packageDir, "..", "..");

  const possiblePaths = [
    join(workspaceRoot, "target", "release", getLibFilename()),
    join(workspaceRoot, "target", "debug", getLibFilename()),
  ];

  for (const path of possiblePaths) {
    if (existsSync(path)) {
      return path;
    }
  }

  return null;
}

function isDevWorkspace(): boolean {
  const packageDir = getPackageDir();
  const workspaceRoot = join(packageDir, "..", "..");
  return existsSync(join(workspaceRoot, "Cargo.toml"));
}

/**
 * Find the native library binary.
 *
 * Resolution order:
 * - FFF_C_LIB (if set) always wins
 * - Dev workspace: local dev build first, then npm package
 * - Production: npm package first, then dev build
 *
 * @returns Absolute path to the library, or null if not found
 */
export function findBinary(): string | null {
  // Explicit override for platforms without a prebuilt package (for example a
  // libfff_c.so built from source on FreeBSD, or any custom build).
  const override = process.env["FFF_C_LIB"];
  if (override) {
    if (!existsSync(override)) {
      throw new Error(`FFF_C_LIB points to a file that does not exist: ${override}`);
    }
    return override;
  }

  if (isDevWorkspace()) {
    // 1. Local bin/ directory (populated by `make prepare-node`)
    const binPath = join(getPackageDir(), "bin", getLibFilename());
    if (existsSync(binPath)) return binPath;

    // 2. Local dev build (target/release or target/debug)
    const devPath = getDevBinaryPath();
    if (devPath) return devPath;

    // 3. Fallback to npm package
    const npmPath = resolveFromNpmPackage();
    if (npmPath) return npmPath;

    return null;
  }

  // Production: npm package first
  const npmPath = resolveFromNpmPackage();
  if (npmPath) return npmPath;

  // Fallback: local dev build (e.g. user built from source)
  return getDevBinaryPath();
}
