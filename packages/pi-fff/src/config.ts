import { readFileSync } from "node:fs";
import { join } from "node:path";
import { piDataDir } from "./paths";

export const CONFIG_FILE_NAME = "pi-fff.json";
export const VALID_MODES = ["tools-and-ui", "tools-only", "ui-only", "override"] as const;

export type FffMode = (typeof VALID_MODES)[number];

export interface FffConfig {
  $schema?: string;
  mode?: FffMode;
  frecencyDbPath?: string;
  historyDbPath?: string;
  enableFsRootScanning?: boolean;
  enableHomeDirScanning?: boolean;
  warnOnHomeDirScan?: boolean;
  followSymlinks?: boolean;
}

const CONFIG_KEYS = new Set<keyof FffConfig>([
  "$schema",
  "mode",
  "frecencyDbPath",
  "historyDbPath",
  "enableFsRootScanning",
  "enableHomeDirScanning",
  "warnOnHomeDirScan",
  "followSymlinks",
]);

export function loadConfig(agentDir = piDataDir()): FffConfig {
  const configPath = join(agentDir, CONFIG_FILE_NAME);
  let contents: string;

  try {
    contents = readFileSync(configPath, "utf8");
  } catch (error: unknown) {
    if ((error as NodeJS.ErrnoException).code === "ENOENT") return {};
    throw new Error(
      `Could not read pi-fff config at ${configPath}: ${errorMessage(error)}`,
    );
  }

  let parsed: unknown;
  try {
    parsed = JSON.parse(contents);
  } catch (error: unknown) {
    throw invalidConfig(configPath, `not valid JSON (${errorMessage(error)})`);
  }

  if (!isRecord(parsed)) {
    throw invalidConfig(configPath, "expected a JSON object");
  }

  for (const key of Object.keys(parsed)) {
    if (!CONFIG_KEYS.has(key as keyof FffConfig)) {
      throw invalidConfig(configPath, `unknown option "${key}"`);
    }
  }

  if (parsed.mode !== undefined && !VALID_MODES.includes(parsed.mode as FffMode)) {
    throw invalidConfig(configPath, `"mode" must be one of ${VALID_MODES.join(", ")}`);
  }

  validateString(configPath, parsed, "$schema");
  validateString(configPath, parsed, "frecencyDbPath");
  validateString(configPath, parsed, "historyDbPath");
  validateBoolean(configPath, parsed, "enableFsRootScanning");
  validateBoolean(configPath, parsed, "enableHomeDirScanning");
  validateBoolean(configPath, parsed, "warnOnHomeDirScan");
  validateBoolean(configPath, parsed, "followSymlinks");

  return parsed as FffConfig;
}

function invalidConfig(configPath: string, reason: string): Error {
  return new Error(`Invalid pi-fff config at ${configPath}: ${reason}`);
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function validateString(
  configPath: string,
  config: Record<string, unknown>,
  key: "$schema" | "frecencyDbPath" | "historyDbPath",
): void {
  const value = config[key];
  if (value !== undefined && (typeof value !== "string" || value.length === 0)) {
    throw invalidConfig(configPath, `"${key}" must be a non-empty string`);
  }
}

function validateBoolean(
  configPath: string,
  config: Record<string, unknown>,
  key:
    | "enableFsRootScanning"
    | "enableHomeDirScanning"
    | "warnOnHomeDirScan"
    | "followSymlinks",
): void {
  const value = config[key];
  if (value !== undefined && typeof value !== "boolean") {
    throw invalidConfig(configPath, `"${key}" must be a boolean`);
  }
}
