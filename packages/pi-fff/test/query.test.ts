import { describe, expect, test } from "bun:test";
import { buildQuery, normalizePathConstraint } from "../src/query";

const cwd = "/tmp/workspace";
const defaultExcludes = [".worktrees/", ".claude/worktrees/"];

describe("path constraint normalization", () => {
  test("converts absolute in-workspace paths to repo-relative constraints", () => {
    expect(normalizePathConstraint("/tmp/workspace/.agents/**", cwd)).toBe(".agents/**");
    expect(normalizePathConstraint("/tmp/workspace/.agents/plans/**", cwd)).toBe(
      ".agents/plans/**",
    );
  });

  test("rejects absolute paths outside the workspace", () => {
    expect(() => normalizePathConstraint("/tmp/other/.agents/**", cwd)).toThrow(
      "Path constraint must be relative to the workspace",
    );
  });

  test("preserves recursive directory globs as root-relative constraints", () => {
    expect(normalizePathConstraint(".agents/**", cwd)).toBe(".agents/**");
    expect(normalizePathConstraint("src/**/*", cwd)).toBe("src/**/*");
    expect(normalizePathConstraint("src/**/*.ts", cwd)).toBe("src/**/*.ts");
    expect(normalizePathConstraint("{src,lib}/**", cwd)).toBe("{src,lib}/**");
  });

  test("builds find queries with normalized include and exclude constraints", () => {
    expect(
      buildQuery("/tmp/workspace/.agents/**", "*", "/tmp/workspace/test/**", cwd),
    ).toBe(".agents/** !test/** *");
  });

  test("treats path='.' as workspace root (no constraint)", () => {
    expect(normalizePathConstraint(".", cwd)).toBeNull();
    expect(normalizePathConstraint("./", cwd)).toBeNull();
    expect(buildQuery(".", "needle", undefined, cwd)).toBe("needle");
  });

  test("treats absolute workspace root as no constraint", () => {
    expect(normalizePathConstraint(cwd, cwd)).toBeNull();
    expect(buildQuery(cwd, "needle", undefined, cwd)).toBe("needle");
  });

  test("bare directory path without trailing slash becomes PathSegment", () => {
    expect(normalizePathConstraint("app", cwd)).toBe("app/");
    expect(normalizePathConstraint("src/nested", cwd)).toBe("src/nested/");
    expect(buildQuery("app", "needle", undefined, cwd)).toBe("app/ needle");
  });

  test("converts absolute in-workspace file path to repo-relative", () => {
    expect(normalizePathConstraint("/tmp/workspace/src/main.rs", cwd)).toBe(
      "src/main.rs",
    );
    expect(buildQuery("/tmp/workspace/src/main.rs", "needle", undefined, cwd)).toBe(
      "src/main.rs needle",
    );
  });

  test("converts absolute in-workspace directory (without trailing slash) to repo-relative", () => {
    expect(normalizePathConstraint("/tmp/workspace/src", cwd)).toBe("src/");
    expect(buildQuery("/tmp/workspace/src", "needle", undefined, cwd)).toBe(
      "src/ needle",
    );
  });

  test("converts absolute in-workspace glob path to repo-relative glob", () => {
    expect(normalizePathConstraint("/tmp/workspace/src/**/*.ts", cwd)).toBe(
      "src/**/*.ts",
    );
  });

  test("bare recursive globs constrain nothing", () => {
    expect(normalizePathConstraint("**", cwd)).toBeNull();
    expect(normalizePathConstraint("**/", cwd)).toBeNull();
    expect(normalizePathConstraint("**/*", cwd)).toBeNull();
    expect(normalizePathConstraint("./**", cwd)).toBeNull();
    expect(buildQuery("**", "needle", undefined, cwd)).toBe("needle");
  });
});

describe("default excludes", () => {
  test("adds configured excludes to ordinary queries", () => {
    expect(buildQuery("src/**", "needle", undefined, cwd, defaultExcludes)).toBe(
      "src/** !.worktrees/ !.claude/worktrees/ needle",
    );
  });

  test("combines configured and caller excludes", () => {
    expect(buildQuery("src/**", "needle", "test/", cwd, defaultExcludes)).toBe(
      "src/** !.worktrees/ !.claude/worktrees/ !test/ needle",
    );
  });

  test("does not apply an exclude when path explicitly targets it", () => {
    expect(
      buildQuery(".worktrees/demo/**", "needle", undefined, cwd, defaultExcludes),
    ).toBe(".worktrees/demo/** needle");
    expect(
      buildQuery(".claude/worktrees/demo/**", "needle", undefined, cwd, defaultExcludes),
    ).toBe(".claude/worktrees/demo/** needle");
  });
});
