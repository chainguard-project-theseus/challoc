# verus-count

The coverage counter of a proved port: it reads the port's sources with
`verus_syn`, classes every executable function and its lines as
verified, trusted, discharged or outside `verus!`, and holds the shipped
crates' shares to `verus/FLOOR`. With its optional checks it also gates
recursion, entry-point reachability (REACH), compile inputs, the C
caller's token mints (MINTS) and rlimit headroom (RLIMIT). It is
pcre3's `verus/count` made generic, with challoc's generated-ledger back
end added; `src/main.rs`'s module documentation states every rule.

## Running

```sh
cargo build --locked --release
verus-count [--config FILE] [--list] [--floor FILE [--base-rev REV | --no-base]] [--rlimit STATS] ROOT
verus-count [--config FILE] --badge ROOT
```

ROOT is the repository root (default `.`) and FILE the configuration
(default `ROOT/verus/count.toml`). `--list` prints one line per
executable function before the table, and changes nothing else: every
check the other options name still runs; `--floor` holds the `shipped` row to the FLOOR
file; `--base-rev` also holds FLOOR (and REACH, RLIMIT and the
configuration) to that revision's copies, and fails when the revision
does not resolve. `--floor` needs one of the two base flags: with
neither the run fails (exit 1), so a CI run that loses its base fails
rather than skip every base comparison. `--no-base` says there is no
base to compare with: a local or development run, or the proof PR that
adds the port's first FLOOR. It is never a CI run's flag once FLOOR is
on the base, and giving it with `--base-rev` fails the run (exit 2);
`--rlimit` reads the statistics `cargo verus ... -- --time-expanded
--output-json` printed. `--version` prints the counter's version.
`--badge` prints the coverage badge line (The badge, below) for the
figures in ROOT's `[floor] path`, and takes no checking option. An
unknown option or a second ROOT fails the run (exit 2), so a misspelt
`--flor` cannot leave the floor unchecked.

## Configuration

`verus/count.toml`, every path relative to the root. Unknown keys are
an error. A check whose section is left out does not run, and prints
nothing. So with `--base-rev REV` the run fails (exit 1) when the
configuration turns off or narrows a gate `REV`'s copy runs: a
`[recursion]`, `[reach]`, `[inputs]`, `[mints]` or `[badge]` section
removed; a `[badge]` `path` moved to another file; a
`[reach]` `exports_macro`, `fn_ptr_macro` or `conversion` removed or
renamed; an `[inputs]` crate or root removed or `lazy` turned off; a
`[crates]` `dir` removed or changed, a member removed, moved to another
`path`, or its `ships` changed to anything but `true` (each compared on
its own, so a member that moves and unships makes two narrowings, and
also by path: whatever the head reads at a crate the base read, a
member under any name or a `dir` it sits in, which reads it with
`ships` unset, so a rename, a member moved onto a crate of the base's
`dir`, and a crate handed between `dir` and members are each compared);
a crate of the base's `dir` (each subdirectory of it with a `src/` in
the base's tree) that the head, with the same `dir`, reads neither at
its directory nor under its name, as a member removed is, or reads under
its name somewhere else, as a member moved is, or reads as a member
with `ships` set to anything but `true` at a path the base's
configuration does not compare;
a `test_cfgs`, `test_features`
or `verus_features` name added; a `verus_sets` or `verus_never_sets`
atom added (compared as atoms, so respacing one is not a change); a
`stand_ins` pair added; or a crate
whose own Cargo.toml gains `publish = false`, where the base read it
with its Cargo.toml deciding whether it ships (a `dir` crate, or a
member without `ships`), at the same directory or under the same name,
and the base's Cargo.toml there does not say so. A crate the base read
under no name and at no directory is new, with nothing to compare: so a
`dir` crate whose directory is renamed is the old crate no longer read
and a new crate, as a member renamed to a new path is. Renaming a table
or a mint is not a narrowing. The base's
copy is read at the configuration's path under ROOT, as the repository
names it (`verus/count.toml` when `--config` names a file outside ROOT);
a base with none, the change that adopts the counter, prints `ok
configuration: REV has no ... to compare with`, and one that does not
parse fails the run (exit 2). A configuration under ROOT reached through
a symlink, the file or a directory above it, fails the run (exit 2),
since the base would be read at the link's path while the head is read
through it. An in-ROOT `--config` at a path the base lacks still has
nothing to compare: that is a Makefile edit, the same file that carries
`--base-rev`.

A narrowing made on purpose (a root that moves with its function, a
crate directory renamed, a gate a port retires) is named the way FLOOR
names a lowered figure: a `[[narrowed]]` line whose `change` is the
narrowing as the failing run prints it, before `, so ...`, and whose
`reason` says why.

```toml
[[narrowed]]
change = "[inputs] roots drops Regex::new in crates/pcre3/src/regex.rs"
reason = "Regex::new moved to builder.rs, which the new root names"
```

The run then prints `ok configuration: ...: <the narrowing>, with a
reason: <reason>`, so review sees it. As with FLOOR's `lowered` lines, a
line the base already has covers nothing new, so a later narrowing of
the same kind needs its own; such a line stays as history and is not
checked. A narrowing made again with the same text (a test cfg dropped
and added back, a section restored and retired again) takes one more
line with that `change` than the base has; a history line reworded is
still history. FLOOR does not count its lines: a figure lowered again to
a value a base `lowered` line already names has no new line to give, so
lowering it there takes an earlier change that removes the old line. A new line that names no narrowing of the base fails the
run (exit 1), and one with no reason does not parse (exit 2).

There is no line for a base copy the counter cannot read: a kit release
that drops or renames a key makes the port's re-sync exit 2 against
every base that still has it. Such a change runs with `--no-base` in
place of `--base-rev`, and then with no base comparison at all, for the configuration, FLOOR,
RLIMIT or REACH, so it needs a maintainer's review in their place.

The base's FLOOR, RLIMIT and REACH are read at the paths the base's
configuration gives them (`[floor] path`, `[rlimit] path`, `[reach]
path`), so moving one of those files, or pointing its key at a path the
base lacks, still compares the head's file with the base's. So that a
base path is always a file some run read: `--floor FILE` must be ROOT's
`[floor] path` (exit 2 otherwise, compared after resolving both), a
`[floor] path` through a symlink fails the run (exit 2), as a
configuration through one does, and a
`[reach]` whose file is missing fails the run (exit 1; the file may
hold no rows). A file whose section the base's configuration has, but
the base lacks, fails the run (exit 2 for FLOOR and RLIMIT, 1 for
REACH). With no base configuration, or no `[reach]` in it, the head's
path is read, and a base without the file prints `ok ...: REV has no
... to compare with`. A crate read twice, under one name or from one
directory, fails the run (exit 2): the tally goes by name. So does a
`[crates]` `dir` or member `path` that is not relative, has a `..` or
passes through a symlink, and a crate whose directory, `src/`, or any
file or directory under `src/` is a symlink (exit 2), since the base
comparison matches crates by path and another spelling of a directory,
or another crate's sources reached through a link, would pass for a new
crate.
[configs/pcre3.toml](configs/pcre3.toml) and
[configs/challoc.toml](configs/challoc.toml) are complete examples.

| section | key | meaning |
|---|---|---|
| `[crates]` | `dir` | each subdirectory with a `src/` is a crate, named for it |
| | `members` | `{ name, path, ships }` crates named explicitly; `ships` overrides the `publish = false` rule (a crate ships unless its Cargo.toml, read as TOML, sets `[package]` `publish = false`; one under `[package.metadata]` does not count) |
| | `test_cfgs` | `cfg` names only test builds set (default `test`, `kani`) |
| | `test_features` | features only tests enable; a workspace build that enables one fails the run (`cargo tree`) |
| | `verus_features` | features only Verus's build enables, as `verus_keep_ghost` |
| | `verus_sets` | `cfg` atoms Verus's build is sure to set, which shipped builds may set too (`'feature = "jit"'` for a default feature the proof run keeps, `"unix"`): "Verus's build", below |
| | `verus_never_sets` | `cfg` atoms Verus's build never sets (`"miri"`), so `not(miri)` is sure there |
| | `stand_ins` | `[from, to]` same-length replacements before parsing, for identifiers Verus keeps as keywords; one that would rewrite a name the counter classes code by (`external_body`, `assume`, `admit`, `verus_verify`, `discharged_by`, `cfg`, `fn`, `unsafe`, a test cfg, a configured macro or mint, ...) is refused |
| | `verus_only_ghost` | count the ghost functions of Verus-only code in the ghost column |
| `[floor]` | `path`, `checked`, `renamed_from` | the FLOOR file (default `verus/FLOOR`); the prefix of the two verified-or-discharged figures (default `checked`); an older prefix the base revision's copy may still give them under, for the change that renames them (challoc's `combined`) |
| `[rlimit]` | `path` | the RLIMIT file (default `verus/RLIMIT`) |
| `[kani]` | `records` | the directory of `kani-*.md` records that cover the `discharged_by = "kani"` marker |
| `[seams]` | `tables`, `theorems` | the seams tables, and a directory whose every `*.md` holds one |
| `[recursion]` | `table` | turns the recursion check on; the table that names verified recursion |
| `[reach]` | `path`, `exports_macro`, `fn_ptr_macro`, `conversion` | turns REACH on; the port's exports macro, function-pointer macro and argument conversion, each optional |
| `[inputs]` | `table`, `crates`, `roots`, `lazy`, `describe` | turns the compile-inputs check on; a root that matches no function of `crates` fails it |
| `[mints]` | `path`, `mint`, `token_trait`, `home` | turns the token-mint gate on |
| `[badge]` | `path` | turns the badge gate on: with `--floor`, the file (default `README.md`) holds FLOOR's figures in one badge line (The badge, below) |
| `[ledger]` | `dir`, `isla`, `kani` | discharges from a generated seam ledger instead of the Kani marker; the premise files an Isla or Kani status may cite |
| `[[narrowed]]` | `change`, `reason` | a narrowing of the base's configuration made on purpose, named as the run prints it, and why |

A minimal configuration is two lines:

```toml
[crates]
dir = "crates"
```

### Verus's build

A function counts as verified only when Verus's build is sure to
compile it. A `cfg` reaches a function from its own attributes, from an
enclosing `impl`, trait, inline `mod` or `verus!` block, from an
out-of-line `#[cfg(..)] mod x;`, which classes `x`'s file and every file
it declares, from an inline module or `verus!` block around such a
declaration (`#[cfg(..)] mod inline { mod x; }`), and from a file's inner
`#![cfg(..)]`, which classes it and every file it declares (a crate
root's, the whole crate). `cfg_attr`s apply as rustc expands them,
nested to any depth (`cfg_attr(P, cfg_attr(R, ..))` applies under
`all(P, R)`) and each attribute of `cfg_attr(P, a, b)` under `P`, for
`cfg`s, `#[path]`s and Verus's attribute form alike: `#[cfg_attr(P,
cfg(Q))]` counts as the `cfg` `any(not(P), Q)`. Every `cfg` a function is under, its own and those
around it, is read as one conjunction. A file declared more than once
(`#[cfg(target_os = "linux")] #[path = "unix.rs"] mod sys;` beside the
same for macos) is built where any declaration holds. A file a `#[path]`
names declares its modules beside it, as a `mod.rs` does. A
`#[cfg_attr(P, path = "..")]` names the module's file where `P` holds
and no earlier path attribute applies, as rustc takes the first in
effect: a bare `#[path]` applies wherever none earlier does and ends the
list, and the default `name.rs` is the file where none applies. An inline
module's directory is chosen the same way. A file declared only
inside another macro's body (`cfg_if! { if #[cfg(windows)] { mod win; }
}`, or the file a `#[path]` there names) is unsure whatever the
configuration settles, since the counter does not evaluate the macro.
So is any crate file but a root that no declaration the counter reads
names: nothing shows where it is built, so it is never verified. The
roots are the targets the crate's Cargo.toml declares (the `[lib]`
`path`, each `[[bin]]`, `[[example]]`, `[[test]]` and `[[bench]]`
`path`), with `src/lib.rs` unless `autolib = false` and, unless
`autobins = false`, the binaries cargo finds (`src/main.rs`,
`src/bin/NAME.rs`, `src/bin/NAME/main.rs`). A root declares its modules
beside it, as `lib.rs` does. A function some shipped build compiles, under a `cfg` Verus's
build might not meet, counts as trusted: it ships, and nothing proves
Verus compiled it. Its proof cannot count, and a shipped function's
proof may rest on it, as on any trusted function (so REACH and the
recursion check treat it as one).

Only the atoms the configuration settles are sure there:
`verus_keep_ghost` and the `verus_features` (set), the test atoms (never
set), and the two lists below. Every other atom, a platform
(`target_os = "windows"`, `unix`), `miri`, or a default feature, may be
either, since the counter cannot tell what the proof run passes. A port
declares what its proof run (the host, the features `make verus`
enables) settles:

```toml
[crates]
dir = "crates"
# The proof run builds the default features, which include jit.
verus_sets = ['feature = "jit"']
# Verus never sets miri.
verus_never_sets = ["miri"]
```

Each entry is one atom, written as a `cfg` writes it (a name, or `name =
"value"`), and the run fails (exit 2) on a predicate (`not(miri)`), and
on an atom the configuration already settles (a test atom, a Verus atom,
or one named in both lists). Adding one is a narrowing (Configuration,
above), since it lets the functions it guards count as verified: name it
with a `[[narrowed]]` line. A feature in `verus_sets` still ships: one
that only Verus's build enables belongs in `verus_features`, which takes
its items out of the shipped tally.

A port that runs Verus on several hosts names each run's atoms (challoc,
which runs it on x86_64 and natively on aarch64: `verus_sets =
['target_arch = "x86_64"', 'target_arch = "aarch64"']`), since a
function under either is checked by the run that sets it.

A predicate that needs two values of one key rustc gives each target
one value of (`target_arch`, `target_os`, `target_env`, `target_abi`,
`target_vendor`, `target_endian`, `target_pointer_width`, `panic`; not
`target_family`, `feature` or `target_feature`) holds in no build,
whatever the atoms say on their own. So with both architectures
declared, an item under `all(target_arch = "x86_64", target_arch =
"aarch64")`, or `all(target_arch = "x86_64", any(target_arch =
"aarch64", test))`, is left out as no build compiles it, and Verus's
attribute form under such a `cfg_attr` never applies, so the function is
outside. The same holds for two `cfg`s that must hold together, on the
function (`#[cfg(target_arch = "x86_64")] #[cfg(target_arch =
"aarch64")]`) or one on it and one around it.

Otherwise the counter reads each atom on its own, not each run's set,
so it cannot tell the runs apart. Where `verus_sets` holds two values of
a single-valued key, a function is sure only if every way the `cfg`s it
is under can hold together needs at most one of the atoms of such keys
true (an atom under an even number of `not`s counts; each alternative of
an `any` is a way of its own, so `any(target_arch = "x86_64",
target_arch = "aarch64")` is sure): `target_arch = "x86_64"` alone is
checked by the run that sets it. Under two, it may ship on a host no run
is, so it counts as trusted. With an
x86_64 Linux run and an aarch64 macOS run (both arches and both oses
declared), `#[cfg(target_os = "macos")] mod mac { #[cfg(target_arch =
"x86_64")] fn f() }` ships on x86_64 macOS, which neither run is, and
`f` is trusted, as it is with the module under `not(not(target_os =
"macos"))`; so is a function under `all(target_arch = "x86_64",
any(target_arch = "aarch64", unix))`. `not(target_arch = "x86_64")`
stays unsure too: the function counts as trusted though the aarch64 run
checks it. A model of each run's atoms would credit these where a run
checks them; the counter has none yet.
Since each atom is read on its own, a file declared under `feature =
"std"` and again under `not(feature = "std")`, which every build
compiles, stays unsure too, unless the configuration settles `std`.

#### Bringing this rule in on a re-sync

The re-sync that brings in this counter carries the port's declarations
in the same PR, each with a `[[narrowed]]` line: with neither list,
every verified function under an unsettled `cfg` turns trusted at once,
and FLOOR (and REACH, for one an entry point reaches) fails on that PR.

- pcre3: `verus_sets = ['feature = "jit"']` and `verus_never_sets =
  ["miri"]`, as [configs/pcre3.toml](configs/pcre3.toml) has. Without
  them, 27 verified functions of pcre3-capi count as trusted
  (functions 91.5% to 90.8%), and REACH fails on exec.rs.
- expat-rs and chainz: none. Neither has a verified function under an
  unsettled `cfg`, and `--list` is unchanged.
- challoc: [configs/challoc.toml](configs/challoc.toml)'s declarations,
  measured on the 11-10 snapshot `0497dc34`; they are re-measured on
  challoc's own re-sync.

A function that stays trusted under honest declarations is verified
more: bring its proof under a `cfg` the proof run settles. Where a
figure still drops, FLOOR takes a `lowered` line with the measured
change and its reason, as for any other drop. Declaring an atom the
proof run does not set, to get the figure back, is the one wrong
answer: it credits a proof no run checked.

An atom applies to every crate under `[crates]`, while a cargo feature
is per crate. `feature = "jit"` in `verus_sets` asserts that the proof
run enables `jit` in every configured crate that has a feature of that
name, so a second crate with a non-default `jit` would be credited
wrongly. In pcre3 only pcre3-capi has `jit`.

## The badge

A port's README shows its Verus coverage as a shields.io badge. The
repositories are private, so shields.io cannot read the figures: they
are written into the line, alt text and URL both, and a badge kept by
hand drifts from FLOOR. With

```toml
[badge]
path = "README.md"
```

a run with `--floor FILE` also fails (exit 1) unless the file holds
exactly one Verus badge, and that line is exactly the one FILE's
`functions` and `lines` figures give, each with one decimal. A Verus
badge is any rendered line holding `verus` in any case together with
`shields.io` or a Markdown image (`![`), so a second badge with a
lowercase label, another shields.io endpoint or an `<img>` tag counts.
Lines inside fenced code blocks (```` ``` ```` or `~~~`) and the text
inside HTML comments (`<!--` to `-->`, across lines) do not render, so
they are neither the badge nor a second one: an example badge in a
fence is not counted, and the canonical line only inside a comment or
a fence fails as no badge. The line FILE's figures give is:

```markdown
[![Verus: 86.3% of functions, 94.4% of lines](https://img.shields.io/badge/Verus-86.3%25%20of%20functions%2C%2094.4%25%20of%20lines-blue)](verus/README.md#coverage)
```

The failures name the line to write:

```text
FAIL badge: README.md has no Verus badge; add this line, which gives verus/FLOOR's functions and lines figures:
FAIL badge: README.md:3 is not the badge verus/FLOOR's functions and lines figures give; replace it with this line:
FAIL badge: README.md has 2 Verus badges (lines 3, 9); keep one, this line:
```

each followed by the line, and a pass prints `ok badge: README.md:3
gives verus/FLOOR's functions and lines figures`. A missing file fails
too (exit 1), and a `path` that is not relative, has a `..` or passes
through a symlink exits 2, as a `[floor] path` through one does. The
badge follows FLOOR, not the counted shares, so it moves only when FLOOR
does: the pull request that raises FLOOR, one of its own (FLOOR's
header), updates the badge too, pasting what `verus-count --badge .`
prints. Without `--floor` the badge
is not checked. Removing `[badge]` or moving its `path` is a narrowing
(Configuration, above). The link is `verus/README.md#coverage` wherever
the file sits, so keep the file at the root.

## Two ways to discharge a seam

- **The Kani marker** (pcre3): a trusted function carries
  `#[cfg_attr(any(), discharged_by = "kani")]` and a `kani-*.md` record
  quotes it. The report is one table with a `discharged` column, and the
  checked figures add the discharged functions. The marker is read through
  `cfg_attr`s as rustc expands them, nested or beside other attributes,
  and counts only where the predicates around it hold in no build
  whatever the configuration declares (`any()`); under one some build may
  meet (`windows`) the function is not discharged.
- **The generated ledger** (challoc, `[ledger]`): `verus/seams.sh`
  writes `verus/ledger/**.rs.md` from the source, and each entry's
  status says whether its seam is discharged, partly checked or
  trusted. The report is challoc's two tables, the second the combined
  share. The marker is refused in this mode, so a function cannot be
  discharged twice.

Verus's attribute form (`#[verus_verify]`, `#[verus_spec(..)]`, alone
or under a `cfg_attr`) counts as code inside `verus!`, for a port whose
MSRV keeps vstd optional (the skill's proofs.md, "When vstd cannot be a
dependency of every build"). Under a `cfg_attr` it is verified only when
Verus's build is sure to meet the predicate ("Verus's build", above): a
configured `verus_features` feature, `verus_keep_ghost`, a `verus_sets`
atom, `not` of a `verus_never_sets` one, or a predicate those settle. A
platform or any other atom the configuration does not settle
(`target_os = "windows"`, or `all(feature = "verus", unix)`) may leave
the attribute off where Verus runs, and the function unchecked, so it
counts as trusted, as a function under a `cfg` Verus might not meet
does. A predicate Verus's build never meets (`any()`, `test`, a
`verus_never_sets` atom) never applies the attribute, so the function is
outside.

## Functions a macro writes

A function an item macro writes is counted like any other, so moving
code into a macro never takes it out of the denominator. An invocation
at item position (in a module, an `impl` or a trait) of a `macro_rules!`
the crate defines is expanded. The definition is one the build compiles
(not one under `#[cfg(test)]` or in a test-only module, `verus!` block
or file), found as rustc finds it. An invocation by a path
(`width::exports!`, `$crate::m!`) takes the definitions that path names:
one `#[macro_export]` puts at the crate root, or one its module
re-exports (`pub(crate) use exports;`, or `pub(crate) use ex as build;`
for `w::build!` and a `*` of `w`), with a first segment the module
imports (`use crate::w as x;` then `x::ex!`) read as each path it
imports, and as written too where a module of the crate has that name
(two under `cfg(unix)` and `cfg(not(unix))` give two candidates). The
crate's `use`s are followed to a fixed point: a module's `use` of a path
that names a definition (`pub(crate) use crate::v::build;`, or `use
crate::w::build as make;`, a rename) gives the definition that module's
path under the name the `use` brings in, and a module's `use p::*;`
gives it each path `p` names there, until no `use` adds one. A `use`
counts only where its own `cfg`s, and its enclosing modules', let it be
built: a test-only `use` imports nothing for a shipped build, and one
under a `cfg` the counter cannot settle is a candidate. An invocation
by a bare
name takes the last definition of that name before it in its file whose
module the invocation is in (a definition in a module that has closed
is out of scope, unless `#[macro_use]` carries it out), and, while that
one is under a `cfg` the counter cannot settle (a platform or a
feature) and so may not be built, each before it back to the first
under no such `cfg`. Failing an unconditional one, it also takes each
definition its module imports by `use` under that name (`use w::ex;`,
or `use w::ex as build;` for `build!`, the imported path read from the
module), each in another file whose textual scope reaches the
invocation (an enclosing module's, or one a `#[macro_use] mod name;`
carries out of its file; not a sibling file's), and, at the crate root,
each `#[macro_export]` puts there. A `macro_rules!` an expansion writes
defines its macro from there on, a later one shadowing an earlier one
as in source. The expansion follows rustc's
rules (each rule tried in order; `ident`, `ty`, `expr`, `block`, `tt`
and the other fragment kinds, a `tt` and a repetition's separator being
one token as rustc's lexer makes them (`<<`, `::`, `'a`), a `literal`
taking `true` and `false` and a `stmt` stopping before a `let`'s `;`; a
fragment other than an `ident`, `lifetime` or `tt` forwarded to another
macro staying one fragment of its kind, which no literal token matches
and only a kind whose parser reads it does, as rustc decides (an `expr`
reads a `literal`, and a `literal` an `expr` that is a literal; a
`path` reads a `ty` that is a path; a `ty` is never an `expr`; a
forwarded `vis`, empty or not, is one `vis`; any other pair rustc's
matcher tries, the counter does not read, and the run fails, saying so:
a `literal` from a forwarded `1 + 2` or `-x`, a `pat` from a forwarded
`expr`, an `item` or `stmt` from a forwarded `vis`; rewrite the macro to
match the fragment as its own kind);
nested repetitions; `$crate`; a `$name` the rule does not bind written
through, as a macro that writes a `macro_rules!` needs; an invocation
the expansion writes is expanded in turn), and each function in it
counts as outside `verus!`, since `verus!` does not expand the macros it
holds.

The fragments match by the crate's edition, as rustc's do. An
`expr_2021` begins at neither `const` nor `_` (and no `expr` at `let`),
and below edition 2024 an `expr` is an `expr_2021`; below 2021 a `pat`
is a `pat_param`, which takes no top-level `|`. The edition is that of
the targets whose files the counter reads: the library (`[lib] edition`,
else the package's), where there is one (a `[lib] path`, or
`src/lib.rs` unless `autolib = false` with no `[lib]`), each binary (each `[[bin]]`'s, else the package's,
and those cargo finds under `autobins`), and any `[[test]]`, `[[bench]]`
or `[[example]]` whose `path` is under `src/`, with `edition.workspace =
true` followed and 2015 when no key is given. The counter does not split
a crate's files by target, so a crate whose read targets sit on both
sides of 2021 or of 2024 fails the run (exit 1); give them one edition
there.

An expansion is read under its invocation's `cfg`s, as code a
module holds is: one under a `cfg` Verus's build might not meet
(`#[cfg(unix)] vone!(v1);`) ships its functions trusted, never
verified, and a definition under such a `cfg` is a candidate, never
dropped. The walks (REACH, recursion, compile inputs, mints) read only the
code outside expansions, with `[reach]`'s `exports_macro` and
`fn_ptr_macro` as before. REACH scans every shipped file for the
exports macro's blocks, found by their tokens (the name, plain or raw,
then `!` and a delimited group, so `exports != n` is none), and fails
closed: a file that does not lex or parse, one that imports the macro
under another name (`use width::exports as wrap;`; a cast such as
`exports as u32` is no import), and one whose
invocations outnumber the blocks the scan reads (an invocation inside
another macro, a function body or an initializer) each fail the run.
Write each block at the top of a module or `verus!` block, under the
macro's own name. The rlimit check reads the base revision's
sources the same way, so a `#[verifier::rlimit]` a macro writes is held
to the base's.

A row lists a written function at the invocation's line. Its lines are
the source lines its tokens come from, in the invocation's file and in
each definition's, so a line added to an export's body in the invocation
adds a line to that export. Two consequences:

- A definition line counts once for each function each invocation
  writes from it: a three-line body in a definition that writes 22
  functions is 66 lines, as 22 hand-written copies would be. Outside
  `verus!` that keeps the denominator honest; once the definition writes
  `verus! { .. }`, its lines count as many times on the verified side,
  which is as many functions as Verus checks.
- A line with no token (a blank line or a comment inside an invocation)
  is no line of a written function, where a hand-written function counts
  every line from `fn` to its closing brace.

An item macro the crate does not define (another crate's) cannot be
expanded, so the invocation counts as one function outside `verus!`,
named `NAME!`, over its lines: visible in `--list`, never missing. Each
of these fails the run (exit 1), as the counter cannot give a count
rustc would agree with:

- an invocation that resolves to none of the crate's definitions of its
  name, or of the name it was imported as, though the crate has one
  (reached by a path the counter does not follow, or a definition an
  expansion in a file read later writes); one whose name the module
  imports from this crate (a `use` path from `crate`, `self`, `super` or
  a module of the crate) by a path that names no definition; one by a
  path into a module of the crate, or a `*` of one, that brings the name
  in by a `use` the counter cannot follow to a definition: invoke it by
  its definition's path, or in that definition's textual scope;
- an invocation one of whose imported paths (a module alias's, or a
  `use` of its name, under `cfg`s the counter cannot settle) names no
  definition by a path the counter follows while another names one:
  the build that takes the first would expand what the counter cannot
  see;
- a macro the crate defines whose invocation no rule matches, or whose
  expansion does not parse;
- more than one definition that can reach the invocation (a pair under
  `cfg`s the counter cannot settle, such as `unix` and `not(unix)`, in
  the invoking file or another) whose expansions count differently (a
  different function, class or line count), or one of whose expansions
  fails to count (an invocation it writes that does not expand or is
  itself ambiguous); ones that count the same are expanded as the first;
- an expansion nested more than 128 deep (rustc's default
  `recursion_limit`);
- a repetition matched in more than 4096 ways, a bound on the search
  that a long `$($t:tt)*` reaches, since each iteration is a way to
  stop.

`count.toml` has no key that lets a run past these: the way forward is
to rewrite the macro so it expands as rustc's would within the bounds
(split a long invocation, keep one definition with the `cfg` inside its
body). chainz, pcre3 and expat-rs come nowhere near the bounds (at most
41 iterations of one repetition, and an expansion two deep).
`thread_local!` writes statics, and counts nothing.

`compile_error!`, which writes nothing that ships, counts nothing
either, but only by a rule true by rustc's own semantics. The counter
does not resolve names; it reads the crate's bindings (its sources' and
every macro expansion's, in a shipped build, `#[macro_use]` read through
`cfg_attr`s too):

- `::core::compile_error!` and `::std::compile_error!`: in edition 2018
  and later a leading `::` names an extern crate, so these count nothing
  unless the crate's edition is 2015 (cargo's default when none is
  given; the lowest of the package's, the workspace's it inherits, and
  any target's own, `[lib] edition`), a root `extern crate .. as core`
  (or `std`) puts another crate under the name, or a Cargo.toml
  dependency of the name is another package (`std = { package = "other"
  }`, in any dependency table, or inherited by `{ workspace = true }`
  from `[workspace.dependencies]`; one the root does not give is taken
  as another). A `--base-rev` walk reads the root Cargo.toml of that
  revision;
- a bare `compile_error!`: counts nothing where nothing in the crate
  binds the name (an item, `macro_rules!`, `use` or `extern crate`, std's
  own `use core::compile_error;` too), no `#[macro_use] extern crate` but
  `std`, `core` or `alloc` is in the crate, no dependency named `std`,
  `core` or `alloc` is another package (whose prelude it would be), and
  no `*` is in its module;
- any other path (`core::compile_error!` without `::`) counts as a
  function.

A binding a macro expansion writes that breaks the rule fails the run
(exit 1), from every candidate definition the counter compares (a pair
under `cfg`s it cannot settle), not only the one it walks. Every other
std macro in item position counts as a function. `global_asm!` always
counts
as one: it can write executable code (a symbol Rust calls through an
`extern` block), which the counter cannot tell from a directive.
`include!` writes what its file holds, and the expression macros
(`asm!`, `concat!`) are not items.

One case stays quiet: a `*` that would bring a crate macro's name in
only through a further `*` the counter cannot follow (`pub use
me::w::*;` through an `extern crate self as me;`). The invocation then
counts as another crate's macro, one `NAME!` row, unless the crate
defines that name, which fails the run as above.

Adopting a kit commit that expands macros lowers a port's shares with no
change to the port, as functions that were in no denominator join it
(chainz from 31.5% to 27.3% of functions, pcre3 from 91.5% to 85.6%), so
the first re-sync fails its FLOOR. A port holds its FLOOR by verifying
more: it brings the macros' bodies inside `verus!` (below), verified or
trusted at a seam, and the re-sync waits until the port's figures hold
its FLOOR. Lowering FLOOR is not the way to adopt a counter change.

To raise the share, put the functions a macro writes inside `verus!`
by writing them there, or have the macro write a `verus! { .. }` block,
which is read as one. The block may name Verus's macro by its path
with a leading `::` (`::vstd::prelude::verus!`,
`::verus_builtin_macros::verus!`), as challoc's `verified_export_pair!`
does. A `verus!` block that may write a function (it holds `fn`) is
Verus's, read in Verus's build:

- with a leading `::`, by the rule above for `vstd` or
  `verus_builtin_macros` (edition 2018 or later, no root `extern crate ..
  as` of the name, no dependency of the name that is another package);
- bare, where nothing in the crate binds `vstd`, `verus_builtin_macros` or
  `verus` but the crate itself (`extern crate vstd;`) or the macro's own
  path (`use vstd::prelude::verus;`), and no `#[macro_use] extern crate`
  but `vstd`, `verus_builtin_macros`, `std`, `core` or `alloc` is in it,
  and no dependency named `vstd` or `verus_builtin_macros` is another
  package; and where no `*` in its module may bring another crate's
  `verus` (a dependency can export its own): a `*` of another crate,
  or of a module of the crate where such a `*` is (`use super::*;`),
  is allowed only beside an import of Verus's in the same module (`use
  vstd::prelude::*;` or `use vstd::prelude::verus;`), which settles the
  name;
- by any other path (`vstd::prelude::verus!` without `::`) never: the run
  fails (exit 1), as it does where a rule does not hold, naming why.

Any other path ending in `verus` (`other::verus!`, `::verus!`) is
another crate's macro.

## Parity

The parity runs below predate macro expansion: a port whose crates
invoke their own item macros (pcre3-capi's `width::exports!` and
`fn_ptr!`) now counts their functions as well, so its table and
`--list` differ by those rows.

Run on the same commits, the kit counter's full output (standard
output, standard error and `--list`) is byte-identical to each port's
in-tree counter, but for one line:

| port | commit | configuration | `--floor verus/FLOOR --base-rev HEAD^` | `--list` |
|---|---|---|---|---|
| pcre3 | pcre3 main `158cf87a` | `configs/pcre3.toml` | identical but the `ok compile inputs` line | identical but that line (3,919 lines) |
| challoc | challoc main `fcde6b2` | `configs/challoc.toml` | identical | identical (1,674 lines) |
| challoc | challoc main `39751a02` | `configs/challoc.toml` | identical (also with no flag, and `--list --floor verus/FLOOR --base-rev fcde6b2`) | identical (1,672 lines) |

The kit's `ok compile inputs` line also names each root it resolved
and where (`(roots Regex::new at crates/pcre3/src/regex.rs:274,
encode_as at crates/pcre3/src/serialize.rs:631)`); pcre3's does not
name them. pcre3's `--rlimit` path, with statistics written for the
purpose (every crate, every function with a `#[verifier::rlimit]`) and
`--base-rev HEAD^`, differs in the same line alone.

Where the counters still differ, on inputs neither port has today:

- pcre3's counter counts the commas of generic arguments in a call's
  arguments (`helper(make::<u8, u16>())`) as the call's, and drops the
  edge to a one-parameter `helper`; the kit leaves such a call's arity
  unknown. pcre3's counter with the same fix gives the same output on
  `158cf87a`.
- pcre3's counter lets a function that keeps its body but loses a
  `#[verifier::rlimit(N)]` below the default rise to the default with
  no `rlimit-raised` line; the kit requires one. pcre3 has no limit
  below the default. (challoc's counter has neither the call walk nor
  the rlimit check.)
- The kit refuses a stand-in that would rewrite a name it classes code
  by, and a compile-inputs root that matches no function of the
  configured crates; it credits Verus's attribute form under a
  `cfg_attr` only when Verus's build is sure to meet the predicate.
  Neither port has a stand-in, root or attribute these refuse.
- The kit counts a function under a `cfg` Verus's build might not meet
  as trusted ("Verus's build"); pcre3's and challoc's counters take
  every atom they do not configure to hold where Verus runs. On pcre3
  main `b5633f1d`, pcre3-capi's `jit` and `miri` guards are the only such
  `cfg`s on a verified or trusted function. With `verus_sets = ['feature
  = "jit"']` and `verus_never_sets = ["miri"]`, as
  [configs/pcre3.toml](configs/pcre3.toml) has, `--list` is the same as
  pcre3's counter's rule gives; without them, 27 verified functions
  of pcre3-capi (exec.rs and tables.rs's `Libc` ctype items) count as
  trusted. On challoc main `0497dc34`, its `target_arch` and
  `loom` guards are the only ones: with
  [configs/challoc.toml](configs/challoc.toml)'s declarations `--list`
  is the same as challoc's counter's rule gives, and without them 7
  verified functions count as trusted.
- With `--base-rev`, the kit also compares `verus/count.toml` with the
  base revision's (Configuration, above), which neither port's counter
  had, and prints one `ok configuration` line first. The rest of the
  output is unchanged.

- challoc's counter takes a missing `combined-*` figure in the FLOOR
  under test as 0.0; the kit's requires all four (a base revision's
  copy may still lack them). challoc's FLOOR has given all four since
  `4f7b840b`, so no base revision a pull request compares with lacks
  them.
- Renaming challoc's `combined-*` figures to `checked-*` (kit/PLAN.md,
  step 2) needs `renamed_from = "combined"` in the change that renames
  them: without it the base revision's copy names figures the
  configuration does not, and the run stops (`unknown figure
  "combined-functions"`, exit 2). With it, on `39751a02` with FLOOR's
  lines renamed, `--floor verus/FLOOR --base-rev HEAD` passes with the
  same four figures, and a `checked-lines` set 0.1 below the base's
  fails for want of a `lowered` line. The key may stay after the rename;
  it only widens what a base's copy may name.
- challoc's counter reads an inline `#[cfg(verus_keep_ghost)] mod { }`
  as shipped code; the kit reads it as Verus-only, as pcre3's does, so
  only its trusted functions count.
- challoc's ledger reads only `src/FILE:LINE` entries; the kit's reads
  any `FILE.rs:LINE`, so a ledger may cover several crates.
- The failure text for a ledger status that names a loader or kernel
  premise without its `callers:` clause, and for an unlisted compile
  input, words its reason from the configuration rather than from
  challoc's and pcre3's file names.

## Bootstrapping a port

A port with no counter and FLOOR 0.0 adopts the counter in its first
proof PR (the skill's lifecycle.md, phase 4):

1. Vendor the counter ("Adopting: vendoring", below), add
   `verus/count.toml` (the two-line form above is enough to start)
   and run the counter in the verus check:
   `verus-count --floor verus/FLOOR --base-rev "$BASE" .`. A base with
   no FLOOR prints `ok coverage floor: ... has no verus/FLOOR to compare
   with`; this PR may instead pass `--no-base`, which no later PR may.
2. Copy `kit/templates/verus/FLOOR`, whose figures are all 0.0.
3. On the PR's base the counter names 0.0 for every figure; on the PR it
   names the figures the proof reaches. Record those in FLOOR in the
   same PR, although FLOOR otherwise rises only in a PR of its own
   (its header): this first raise cannot come before the counter and the
   proof.
4. Add `[badge]` to `verus/count.toml` and put the line
   `verus-count --badge .` prints on the README's third line, under
   its title (The badge, above).

A port with a `verus/floor/` directory names the FLOOR file something
else (chainz: `verus/SHARES`), since `verus/FLOOR` and `verus/floor/`
are one path on a case-insensitive file system. Name it in both places:
the `--floor` flag, which the counter reads at the head, and
`[floor] path` in `verus/count.toml`, which it reads at `--base-rev`
(chainz: `--floor verus/SHARES` and `path = "verus/SHARES"`). With only
the key set, step 1's `--floor verus/FLOOR` reads the directory and the
counter exits 2.

On a chainz pull request's head (`eb79512`), the two-line configuration counts
16 verified functions in zlib-core, the number chainz's own
`verus/FLOOR` records (`verified 16`, an older single-figure format
the kit does not read), and names `functions 4.1`, `lines 12.8`. On
expat-rs's U1 worktree, with `verus_features = ["verus"]`, it counts
`utf8::invalid_char` (57 lines) as verified, the only one, as the
pilot's hand count did.

## Adopting: vendoring

TODO: once proved-port (and the ports) are public, publish verus-count as a package (crates.io or a Wolfi apk installed as a pinned environment package) and replace vendoring.

A port's check guests cannot reach this repository, so a port carries
a copy of this crate at `verus/count/`, made by `kit/sync.sh` from a
clone of this repository:

```sh
kit/sync.sh PORT_ROOT          # copy kit/count at HEAD to PORT_ROOT/verus/count
kit/sync.sh --check PORT_ROOT  # fail unless the copy matches its recorded commit
```

- The copy is `git archive` of `kit/count` at the clone's HEAD, so it
  holds exactly what that commit holds: no `target/`, no uncommitted
  edit (the sync refuses while `kit/count` has any), and none of the
  files a later kit deletes. It replaces the whole directory.
- `verus/count/KIT` records the commit, and the tag when HEAD has one.
  The sync refuses a `verus/count/` with no `KIT` file: that is the
  port's own counter, for the port's PR to delete.
- `--check` fails when a file is added, deleted or edited or changes
  its executable bit, or when the recorded commit or tag does not
  resolve in the clone; it ignores `verus/count/target/`. The port's CI
  cannot reach this repository to run it, so whoever changes the copy
  runs it and names the commit in the PR.
- The crate builds where it lands: it is its own workspace (the empty
  `[workspace]` table) with its own `Cargo.lock`, and nothing in it
  names a path outside it. The port's verus check builds it with
  `cargo build --locked --release --manifest-path verus/count/Cargo.toml`
  (a `--target-dir` outside the copy keeps `git status` clean) and runs
  it as Bootstrapping, step 1, says.
- Moving to a newer kit is a port PR that re-runs the sync. Never edit
  the copy: a fix the port needs lands in `kit/count` first.
- The same sync copies `kit/make` to `verus/make/`, which `KIT` and
  `--check` cover too: `proved-port.mk`, which the port's Makefile
  includes for `make verus` (the build and run above) and `make kani`;
  their scripts; and the templates of the kit's Verus files
  (`kit/make/VERUS-FILES`), which `make verus-headers` holds the port's
  copies to. `kit/make/proved-port.mk`'s header names each target and
  variable.

## Release

Ports will pin the kit as a released package (kit/PLAN.md, section 4)
once the TODO above is done; until then they vendor it. A
release of this crate needs:

- a version: `Cargo.toml` says `0.0.0` until a `vX.Y.Z` tag
  sets it, and `--version` prints it so a port's Makefile can refuse
  another;
- a package build (a melange config in a package repository the ports'
  check guests can read) that runs `cargo build --locked --release`
  here and installs `verus-count`, with `cargo test` in its test
  stage;
- the ports' checks to install it pinned with `~`, and to drop the
  in-tree counter and its `counter tests` step in the same PR.

No tag or release exists yet.
