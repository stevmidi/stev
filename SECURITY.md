# Security Policy

## Reporting a vulnerability

Please don't open a public issue for a security problem. Report it privately
instead, through GitHub's
[private vulnerability reporting](https://github.com/stevmidi/stev/security/advisories/new)
(the **Report a vulnerability** button on the repository's Security tab).

Include what you can of:

- What the problem is and what an attacker could do with it.
- Steps or a file that reproduces it: a `.stev` project, a `.mid` file, or
  whatever else triggers it.
- The Stev version or commit, your macOS version, and your Mac (Intel or Apple
  Silicon).

Stev is a one-person project, so this is best effort. Expect an
acknowledgement within a couple of weeks. Once a fix is released, the advisory
is published, crediting you unless you'd rather not be named.

## Supported versions

Only the latest release gets security fixes. There are no backports to older
versions.

## Scope

**In scope:** anything where a file or input Stev reads makes it do something
it shouldn't, such as running code, writing outside where it should, or
memory corruption. That covers project files (`.stev`), Standard MIDI Files
imported into Stev, the plugin scan cache, and incoming MIDI.

**Out of scope:**

- **Plugins themselves.** A CLAP or VST3 plugin runs inside Stev's process
  with the same rights as Stev, as it does in any host. Loading a plugin means
  trusting it, so a malicious plugin isn't a Stev vulnerability. A bug in how
  Stev *hosts* plugins that a well-behaved plugin can trigger is in scope.
- A plain crash from a malformed file with no further impact. It's still worth
  fixing, so report it as an ordinary [issue](https://github.com/stevmidi/stev/issues).
- Linux and Windows builds, which are untested (see
  [CONTRIBUTING.md](CONTRIBUTING.md#linux-and-windows)). Reports are welcome,
  but fixes may be slow.
