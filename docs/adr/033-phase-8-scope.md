# 033 — Phase 8 scope: what its gate means before text exists

- **Status:** accepted for items (a)-(d) and (f); item (e) is accepted as a
  test plan but raises one question for the user, below, and waits.
- **Date:** 2026-11-04
- **Phase:** 8
- **Invariants touched:** none. Like ADRs 019, 029 and 031, this fixes what a §9
  gate item means, so it is written down before the gate tests are.

## Context

§9 gives Phase 8 four gate items: "25 static pages render headless; pixel-diff
baselines committed; GPU process proven to have no filesystem or network
capability; killing it falls back to software without losing the session."
Four things about the tree as it stands make each item ambiguous.

- **There is no text.** Shaping is Phase 9. A page "renders" with its text as
  coloured blocks, and Phase 9 will change every baseline that contains text.
- **There is no corpus of 25 pages.** `tests/compat/` holds only `sites.toml`,
  which is empty by design: its header forbids naming sites and forbids
  committing archives (the repository is public, ADR 002). `tests/net/corpus/`
  holds 200 HARs, but they are synthetic fetch-framing fixtures produced by
  `make-fetch-corpus` (`<html><body>resource 0</body></html>`, with a
  `_replay.shape` field); none is a real site. So **no recorded real-site archive
  can be committed or replayed from this repository**, and this ADR does not
  pretend otherwise. The vendored corpora that do exist are `tests/wpt/css2`
  (105 reftest pairs, 210 files), `css-flexbox` and `css-grid`, each at a pinned
  WPT commit.
- **The sandbox does not yet provide what the gate names.** On Linux
  `px-sandbox` applies `no_new_privs` plus a seccomp *denylist* of 17 escape
  primitives (`ptrace`, `mount`, `bpf`...). `open`, `openat`, `socket` and
  `connect` are not denied; Landlock and user namespaces are named in ADR 007
  as rungs but are not applied by `spawn` (`applied` is `[NoNewPrivs, Seccomp]`).
  On Windows it is a restricted token and a job object. "No filesystem or network
  capability" is therefore a Phase 8 *deliverable* in `px-sandbox`, not a property
  to assert about code that already exists.
- **The IPC frame limit is 1 MiB** (`MAX_MESSAGE_BYTES`). One 800x600 RGBA frame
  is 1.83 MB.

`crates/px-paint` already holds the display-list types (commit 0b38428); its
manifest cites this ADR.

## Decision

**(a) "25 static pages render headless" means 25 committed fixture pages,
rendered by the software rasterizer.** `tests/render/pages.toml` lists them;
each entry names its source and its viewport. The mix:

- **13 hand-written feature fixtures**, one idea each, written for this gate:
  backgrounds and nesting; each border side, widths and `none`; `overflow` clip
  nested two deep; z-order of positioned boxes; floats; margin collapse made
  visible by backgrounds; inline-block runs and the text placeholder at several
  sizes; a flex row, a flex column with wrap; a grid with spans; a page taller than
  the viewport; an empty document; a `display:none` subtree; a box wider than the
  viewport.
- **12 pages derived from the vendored WPT corpus**: 6 from `tests/wpt/css2`, 3
  from `css-flexbox`, 3 from `css-grid`. Selection is by rule, not by eye: list the
  reftest pairs in sorted path order, keep those whose test and reference both
  lay out without error and whose rendered bitmaps are *identical* (a reftest
  passing is evidence the pixels mean something), and take every k-th pair, with
  k chosen so the directory contributes its quota. The manifest records the WPT
  commit, the path and k. Nothing is picked because it looks good, which is the
  failure ADR 019 describes in a different gate. For these 12 the gate also
  asserts test-bitmap == reference-bitmap, which is a pixel-level reftest for
  free.
- **No real-site archive.** For the reason above, none of the 25 is one. The
  recorded-traffic archives for real sites live outside this repository (§8), and
  the compat suite will exercise them when it exists. Phase 8 claims nothing
  about real sites.

The viewport is fixed at 800x600; page height is the document height, capped at
2,400 CSS px so a runaway page cannot produce an unbounded bitmap.

**(b) "Pixel-diff baselines committed" means committed PNGs and an exact-match
comparison.** Baselines live in `tests/render/baselines/<id>.png`. The software
rasterizer is integer-only (Au to device-pixel rounding happens once; no floating
point, no anti-aliasing in Phase 8), so its output is identical on Windows and
Linux and **the tolerance is 0**. Any differing byte, or any difference in
dimensions, fails the gate; the failure writes the actual and a diff image to
`target/render-diff/` and names the first differing pixel. One baseline set serves
both operating systems, which is what makes the Linux and Windows CI jobs a
cross-platform determinism test and not two private truths.

Baselines change only by `cargo xtask render --bless`, which rewrites the PNGs,
refuses to run when `CI` is set, and prints the list of files it changed. A
blessed change is reviewed as a PNG diff in the pull request, with the reason in
the commit message. This is the same shape as `cargo xtask unsafe-audit --write`:
the update is deliberate, separate and visible. Nothing in a test or a gate writes
a baseline as a side effect.

The PNG codec is first-party and in the tree, not the `image` crate: a baseline
format that needs a dependency closure to write is the wrong first use of one,
and Phase 15 owns real image decoding behind its own fuzzing. The encoder writes
8-bit RGBA, no interlace, fixed-Huffman deflate with a deterministic greedy
matcher, so flat-colour pages stay in the tens of kilobytes and the same bitmap
always produces the same bytes. The decoder is a full inflate, so a PNG made by
any tool can be compared.

**(c) Text paints as placeholder blocks until Phase 9, and the gate report says so
in its first paragraph.** `Item::TextRun` is a solid rectangle in the text colour.
Phase 8 does not claim to render text. **Known cost, stated now: every baseline
containing a text run will be regenerated when Phase 9 lands**, in a single
reviewed `--bless` commit. The Phase 9 plan must budget for that diff; it is not a
regression when it happens.

**(d) The pipeline is layout, then display list, then backend.** `px-paint` turns
a laid-out document into a `DisplayList`; a backend turns a `DisplayList` and a
size into a bitmap. The backend is a trait in `px-gpu` with exactly two
implementations: `SoftwareRasterizer` first, `WgpuRasterizer` second. Both consume
the same list, which is what makes the GPU output comparable to the software
output. The broker does not call the trait directly for the GPU. A `GpuSession`
supervises the GPU process, sends it display lists, and on any failure renders
the same list with `SoftwareRasterizer`. The supervisor is not a third backend;
it owns no pixels.

**(e) The GPU-process gate items are proven on the process boundary.** Concretely:

1. `px-sandbox` gains a stricter profile for a `px-gpu` child: a seccomp rule
   set that adds `open`, `openat`, `openat2`, `socket`, `connect`, `bind`,
   `execve` and the directory-walking calls to the deny list, installed *after*
   the process has finished starting, plus Landlock with an empty ruleset where
   the kernel has it; on Windows a restricted token with no capabilities, a job
   object, and the process-creation mitigation that blocks child processes. Each
   applied rung is recorded in `SandboxedChild::applied`, as today. How a
   process that must exec and dynamically link starts under a policy that denies
   `open` is a design problem for the unit that builds it (candidate: a static
   binary, or a two-stage policy where stage one is applied at creation and stage
   two only tightens); it is not decided here.
2. A `testing`-feature-only probe mode in the `px-gpu` binary (§14.4; scanned for
   in release artifacts by the existing symbol scan) accepts one request: attempt
   `open("/etc/passwd")`, `openat` on the working directory, `socket(AF_INET)`,
   `connect`, and on Windows `CreateFileW` and `WSASocketW`; reply with each
   result. The test, in `px-broker`, spawns it through `px_sandbox::spawn` and
   asserts every attempt failed with a denial error or that the child was killed
   by the filter. The child's report is evidence about its behaviour, never about
   its authority (invariant 9); the `applied` list, which the child cannot write,
   is asserted alongside it.
3. **Kill and fall back.** A broker test starts a `GpuSession`, renders page A and
   compares to its baseline, kills the GPU process mid-session (`kill`, and
   separately by sending it a malformed frame), renders page B, and asserts: B is
   byte-identical to its software baseline; the session's frame tree, `ChannelId`s
   and tab state are the ones from before; the broker's audit log shows the
   process's death and nothing else changed. A restart is attempted up to the
   existing `MAX_RESTARTS`, after which the session stays on software for its
   lifetime and says so.
4. **Frame transport.** A frame exceeds `MAX_MESSAGE_BYTES`, so the GPU process
   returns it in row strips of at most 256 KiB, each a bounded message, reassembled
   and size-checked by the broker. In-band handle passing for a shared surface,
   which ADR 008 earmarked for Phase 8, is not needed by a headless render and
   waits for the live window (Phase 18).

**What can be proven only on a real GPU, which this container does not have:**
that `WgpuRasterizer` creates a device at all under the profile in item 1; that
its output for the 25 pages is within a stated tolerance of the software
baseline (GPUs are not bit-exact with each other; for axis-aligned integer-snapped
rectangles it may be exact, to be measured, and the tolerance is a number recorded
from that measurement, not guessed here); and that a real driver loss is survived.
Those tests exist, are marked `#[ignore]` with a reason, and run on a machine the
maintainer has. CI on a runner without a GPU runs everything in items 1-4 against a
child whose backend is the software rasterizer, which proves the *boundary* and
says nothing about wgpu. The gate report states this split.

**Decided by the user, 2026-11-04: the gate is read as "no ambient filesystem access"**
(below), and `crates/px-gpu/CLAUDE.md` is reworded to match. The question as raised:

A real Vulkan or DX12 process cannot have "no filesystem capability" in the
literal sense. The Vulkan loader `dlopen`s `libvulkan.so.1`, reads ICD manifests
under `/usr/share/vulkan/icd.d`, and Mesa opens `/dev/dri/renderD*`, `/sys`
entries and a shader cache; DX12 loads `d3d12.dll` and the driver's user-mode DLLs
(`wgpu-hal` calls `libloading::Library::new` for these). What can be achieved is
**no *ambient* filesystem access**: the broker opens the render node and passes it,
driver paths are granted read-only by Landlock rules and nothing else, and no
network. That is a weaker and more honest sentence than the gate's. I propose the
gate be read that way, and that `crates/px-gpu/CLAUDE.md` be reworded to match. I
have not made the change.

**(f) Out of Phase 8, explicitly:** text and fonts (Phase 9); images, `<img>`,
`background-image` (Phase 15); gradients; transforms, `opacity`, `filter`,
`mix-blend-mode`; rounded corners and `border-radius` (anti-aliasing breaks
exactness); border styles beyond `none` and `solid`; box and text shadow; canvas;
video; scrolling and compositing layers beyond one flat list with clip push/pop;
damage tracking; a live window or swapchain; GPU acceleration of anything not in
the display list's five items. A page using an unsupported property renders
without it, and the gate report lists which of the 25 do so.

## Alternatives rejected

- **A committed real-site archive.** `tests/compat` has none and its own header
  forbids committing them; the synthetic HARs are not pages. Inventing one would be
  a fixture dressed as a site.
- **Tolerance in the pixel comparison** (a few percent, or per-channel slack).
  The software rasterizer is deterministic; slack would hide the first genuine
  regression, and "how much slack" has no principled answer. Tolerance belongs to
  the GPU comparison, where it is needed.
- **Per-platform baselines**, as Phase 9 will need for fonts. With integer-only
  rasterization there is nothing platform-specific, and two sets would let the
  platforms drift apart unnoticed.
- **The `image` crate for PNG.** Legitimate under §3, but a dependency closure and
  an ADR to write 25 test files; Phase 15 will take that decision with a fuzzing
  budget behind it.
- **Wait for Phase 9 and render real text.** Leaves Phase 8 unable to test its own
  boundary.
- **A backend enum instead of a trait.** Either works. A trait lets the test
  harness supply a deliberately failing backend to exercise fallback without
  killing a process.

## Consequences

Every text-bearing baseline is regenerated in Phase 9 (stated in (c)). The 25-page
gate says nothing about real-site fidelity. Writing the stricter sandbox profile
puts new unsafe code in `px-sandbox`, the crate with the highest review cost, and
it needs `// SAFETY:` comments against the OS documentation and a session of
adversarial review (§10). The GPU half of the gate is not provable in CI here, and
the report will say "boundary proven, wgpu unproven on this runner" rather than
"gate passed". The PNG codec is 400-600 lines of first-party code that needs its
own tests, including a round trip and a corrupt-input test.

## Verification

Wrong if: the 25 pages cannot detect a deliberate one-pixel rasterizer change
(mutate a border width by one device pixel and the gate must fail); or the Linux
and Windows software renders differ by a single byte (then "integer-only" was not
true, and the cause is the finding); or the stricter profile cannot be made to
coexist with process startup, in which case (e)1 is revised, not the gate.
