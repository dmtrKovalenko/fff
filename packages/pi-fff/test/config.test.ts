import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";

import { CONFIG_FILE_NAME, loadConfig } from "../src/config";

describe("loadConfig", () => {
  let agentDir: string;
  let configPath: string;

  beforeEach(() => {
    agentDir = fs.mkdtempSync(path.join(os.tmpdir(), "pi-fff-config-"));
    configPath = path.join(agentDir, CONFIG_FILE_NAME);
  });

  afterEach(() => {
    fs.rmSync(agentDir, { recursive: true, force: true });
  });

  test("returns an empty config when the file does not exist", () => {
    expect(loadConfig(agentDir)).toEqual({});
  });

  test("loads every supported option", () => {
    const config = {
      $schema:
        "https://raw.githubusercontent.com/dmtrKovalenko/fff/main/packages/pi-fff/pi-fff.schema.json",
      mode: "override" as const,
      frecencyDbPath: "/data/frecency",
      historyDbPath: "/data/history",
      enableFsRootScanning: true,
      enableHomeDirScanning: false,
      warnOnHomeDirScan: false,
      followSymlinks: true,
    };
    writeConfig(config);

    expect(loadConfig(agentDir)).toEqual(config);
  });

  test("accepts ui-only mode", () => {
    writeConfig({ mode: "ui-only" });

    expect(loadConfig(agentDir)).toEqual({ mode: "ui-only" });
  });

  test("rejects malformed JSON", () => {
    fs.writeFileSync(configPath, '{"mode":');

    expect(() => loadConfig(agentDir)).toThrow(
      `Invalid pi-fff config at ${configPath}: not valid JSON`,
    );
  });

  test("rejects non-object config", () => {
    writeConfig(["override"]);

    expect(() => loadConfig(agentDir)).toThrow("expected a JSON object");
  });

  test("rejects unknown options", () => {
    writeConfig({ mode: "override", typo: true });

    expect(() => loadConfig(agentDir)).toThrow('unknown option "typo"');
  });

  test("rejects invalid option values", () => {
    const cases: [Record<string, unknown>, string][] = [
      [{ $schema: false }, '"$schema" must be a non-empty string'],
      [{ $schema: "" }, '"$schema" must be a non-empty string'],
      [{ mode: "replace" }, '"mode" must be one of'],
      [{ frecencyDbPath: "" }, '"frecencyDbPath" must be a non-empty string'],
      [{ historyDbPath: false }, '"historyDbPath" must be a non-empty string'],
      [{ enableFsRootScanning: 1 }, '"enableFsRootScanning" must be a boolean'],
      [{ enableHomeDirScanning: "false" }, '"enableHomeDirScanning" must be a boolean'],
      [{ warnOnHomeDirScan: "false" }, '"warnOnHomeDirScan" must be a boolean'],
      [{ followSymlinks: "true" }, '"followSymlinks" must be a boolean'],
    ];

    for (const [config, message] of cases) {
      writeConfig(config);
      expect(() => loadConfig(agentDir)).toThrow(message);
    }
  });

  test("reports file read failures", () => {
    fs.mkdirSync(configPath);

    expect(() => loadConfig(agentDir)).toThrow(
      `Could not read pi-fff config at ${configPath}`,
    );
  });

  function writeConfig(config: unknown): void {
    fs.writeFileSync(configPath, JSON.stringify(config));
  }
});
