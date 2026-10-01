// A host of this suite's own, serving a shared skill library the caller owns.
//
// Every other Skills spec drives the one managed host, whose library is the
// repository's `companies/` tree — read once per process and cached, so nothing
// a spec can do makes the library move under an install. That is the whole
// reason `updateAvailable` had no end-to-end coverage: producing it needs the
// library to change *between* two reads of it, and a read only happens at boot.
//
// A second process is the missing degree of freedom. The host takes its library
// from the parent of whatever `--company` names, and treats any subdirectory
// there holding `skills/<slug>/SKILL.md` as a bundle — no `company.toml`
// required. So a temporary tree of
//
//   <root>/company/      the served company, copied from companies/e2e_harness
//   <root>/library/      one skill, the entire registry this host serves
//
// gives a spec a library it authors. Install from it, stop the host, rewrite
// the file, start again over the SAME data root: the pin is older than the
// library, and the host computes real drift from real digests.
//
// Nothing here is shared with the managed host — its own port, its own data
// root, its own company copy — so a spec using this cannot disturb the rest of
// the run, and the rest of the run cannot disturb it.

import { spawn, type ChildProcess } from "node:child_process";
import { createServer } from "node:net";
import {
  cpSync,
  mkdirSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { expect, type APIRequestContext, type Browser } from "@playwright/test";

const here = dirname(fileURLToPath(import.meta.url));
const repoRoot = resolve(here, "../../..");

/** The admin `companies/e2e_harness` names, and therefore the copy's admin. */
const ADMIN_EMAIL = "harness-e2e@tinyhumans.ai";

/** A port nothing is listening on, claimed and released back to the OS. */
async function freePort(): Promise<number> {
  return new Promise((ok, fail) => {
    const probe = createServer();
    probe.once("error", fail);
    probe.listen(0, "127.0.0.1", () => {
      const address = probe.address();
      if (address === null || typeof address === "string") {
        probe.close(() => fail(new Error("no port was assigned")));
        return;
      }
      const { port } = address;
      probe.close(() => ok(port));
    });
  });
}

/** A host of our own, and the tree it serves. */
export class DriftHost {
  private process: ChildProcess | null = null;

  private constructor(
    readonly root: string,
    readonly port: number,
  ) {}

  get url(): string {
    return `http://127.0.0.1:${this.port}`;
  }

  /** Where the library's copy of `slug` lives, for a spec to rewrite. */
  skillFile(slug: string): string {
    return join(this.root, "library", "skills", slug, "SKILL.md");
  }

  /**
   * Lays out the tree and picks a port. Nothing is running yet.
   *
   * `library` holds one skill copied out of the repository, so the spec starts
   * from a document the host already validates rather than one written here.
   */
  static async create(label: string, seed: { slug: string; from: string }) {
    const root = join(repoRoot, "target/e2e", `drift-${label}-${process.pid}`);
    rmSync(root, { recursive: true, force: true });
    mkdirSync(join(root, "library", "skills", seed.slug), { recursive: true });
    mkdirSync(join(root, "data"), { recursive: true });
    cpSync(join(repoRoot, "companies/e2e_harness"), join(root, "company"), {
      recursive: true,
    });
    cpSync(
      join(repoRoot, seed.from),
      join(root, "library", "skills", seed.slug, "SKILL.md"),
    );
    return new DriftHost(root, await freePort());
  }

  /**
   * Starts the host and waits for it to answer.
   *
   * `env` is built rather than inherited, for `host.sh`'s reason: an
   * `OPENCOMPANY_PUBLIC_URL` or a mail transport left in the caller's shell
   * stops the login code being echoed, and the sign-in below is the only way
   * into this host.
   */
  async start(): Promise<void> {
    if (this.process) throw new Error("this host is already running");
    const binary =
      process.env.PW_HOST_BINARY ?? join(repoRoot, "target/debug/opencompany");
    const child = spawn(
      binary,
      [
        "serve",
        "--bind",
        `127.0.0.1:${this.port}`,
        "--company",
        join(this.root, "company"),
      ],
      {
        cwd: repoRoot,
        stdio: ["ignore", "pipe", "pipe"],
        env: {
          HOME: process.env.HOME ?? "",
          PATH: process.env.PATH ?? "",
          TMPDIR: process.env.TMPDIR ?? "",
          OPENCOMPANY_CONSOLE_DIR: join(repoRoot, "frontend/dist"),
          OPENCOMPANY_DATA_DIR: join(this.root, "data"),
          OPENCOMPANY_SKIP_ACTIVATION_GATE: "1",
        },
      },
    );
    this.process = child;

    const log: string[] = [];
    child.stdout?.on("data", (chunk) => log.push(String(chunk)));
    child.stderr?.on("data", (chunk) => log.push(String(chunk)));

    const deadline = Date.now() + 60_000;
    for (;;) {
      if (child.exitCode !== null) {
        throw new Error(
          `the drift host exited with ${child.exitCode} before it answered:\n${log.join("")}`,
        );
      }
      try {
        const answer = await fetch(`${this.url}/healthz`);
        if (answer.ok) return;
      } catch {
        // Not listening yet.
      }
      if (Date.now() > deadline) {
        throw new Error(`the drift host never answered:\n${log.join("")}`);
      }
      await new Promise((ok) => setTimeout(ok, 250));
    }
  }

  /** Stops the host and waits for the port to be given up. */
  async stop(): Promise<void> {
    const child = this.process;
    if (!child) return;
    this.process = null;
    const ended = new Promise<void>((ok) => child.once("exit", () => ok()));
    child.kill("SIGTERM");
    await Promise.race([
      ended,
      new Promise<void>((ok) =>
        setTimeout(() => (child.kill("SIGKILL"), ok()), 10_000),
      ),
    ]);
  }

  /** Stops the host and removes everything it was given. */
  async dispose(): Promise<void> {
    await this.stop();
    rmSync(this.root, { recursive: true, force: true });
  }

  /** Rewrites the library's copy of `slug`, which only a restart is read by. */
  publish(slug: string, edit: (doc: string) => string): void {
    const path = this.skillFile(slug);
    writeFileSync(path, edit(readFileSync(path, "utf8")), "utf8");
  }

  /** A browser context signed in to THIS host as its admin. */
  async signIn(browser: Browser) {
    const context = await browser.newContext({
      baseURL: this.url,
      storageState: undefined,
    });
    await signInAsAdmin(context.request);
    return context;
  }
}

/** Redeems an echoed login code, the way `global-setup.ts` does for the suite. */
async function signInAsAdmin(request: APIRequestContext): Promise<void> {
  const requested = await request.post("/api/v1/company/auth/request", {
    data: { email: ADMIN_EMAIL },
  });
  expect(
    requested.ok(),
    `sign-in request failed: ${await requested.text()}`,
  ).toBeTruthy();
  const devCode = ((await requested.json()) as { dev_code?: string }).dev_code;
  expect(
    devCode,
    "no dev_code came back, so this host cannot be signed in to — it must bind " +
      "loopback with no OPENCOMPANY_PUBLIC_URL and no mail transport",
  ).toBeTruthy();
  const verified = await request.post("/api/v1/company/auth/verify", {
    data: { code: devCode },
  });
  expect(
    verified.ok(),
    `sign-in failed: ${await verified.text()}`,
  ).toBeTruthy();
}
