# cardcast-bin on the AUR

`cardcast-bin/` is the AUR package: it installs cardcast from the Linux tarball a `v*` tag attaches
to the GitHub release (`.github/workflows/release.yml`). The `-bin` suffix marks a prebuilt
package whose sources are available ([AUR submission guidelines][rules]). CI's `aur` job builds it
with `makepkg` and runs `namcap` in an Arch container against the tarball it just built
(`.github/ci/aur.sh`). Only the owner pushes to the AUR.

## First push (once)

1. Make an account on <https://aur.archlinux.org/register>.
2. Make an SSH key for the AUR only ([Authentication][auth]): `ssh-keygen -f ~/.ssh/aur`, paste
   `~/.ssh/aur.pub` into **My Account** > **SSH Public Key**, and add to `~/.ssh/config`:

   ```
   Host aur.archlinux.org
     IdentityFile ~/.ssh/aur
     User aur
   ```

3. Make a release as in "Every release" below, steps 1 and 2. The PKGBUILD must point at the first
   release with a Linux tarball, and v0.1.0 has none, so this needs a version bump (say `v0.2.0`).
4. Clone the empty package and copy the files in ([Creating package repositories][clone]):

   ```
   git -c init.defaultBranch=master clone ssh://aur@aur.archlinux.org/cardcast-bin.git
   cp packaging/aur/cardcast-bin/{PKGBUILD,.SRCINFO,LICENSE} cardcast-bin/
   cd cardcast-bin
   ```

5. Set the checksum and push (as in "Every release" below, steps 3 to 5).

## Every release

1. Before tagging, in one commit in this repo: set `version` in `Cargo.toml` (and `Cargo.lock`, with
   `cargo build`), `pkgver` in `cardcast-bin/PKGBUILD` to the same version with `pkgrel=1`, and
   `.SRCINFO` with `makepkg --printsrcinfo > .SRCINFO`. CI's `aur` job (`aur.sh`) fails when they
   disagree. A PKGBUILD fix with no new release raises `pkgrel` instead ([Publishing][publish]).
2. Push it, and when CI is green tag that commit `v<version>` and push the tag. The Release
   workflow attaches `cardcast-v<version>-linux-x86_64.tar.gz`. It never replaces a file a release
   already has, so a fixed build needs a new version. To try the workflow without a tag,
   `gh workflow run release.yml --ref <branch>` makes a draft release `dry-run-<run id>` with the
   same files; delete it after with `gh release delete dry-run-<run id>`.
3. In the AUR clone, copy in `PKGBUILD` and `.SRCINFO` from this repo, then `updpkgsums` (from
   `pacman-contrib`) downloads the release tarball and writes its sum into `sha256sums_x86_64`; it
   must match the release's `.sha256` file.
4. `makepkg -si` to build and install it, then `makepkg --printsrcinfo > .SRCINFO`.
5. `git add PKGBUILD .SRCINFO LICENSE && git commit -m "Update to <version>" && git push` (the AUR
   takes pushes to `master` only).
6. Copy `PKGBUILD` and `.SRCINFO` back into this repo's `packaging/aur/cardcast-bin/`.

The committed `sha256sums_x86_64=('SKIP')` stands until step 6 of the first release; CI sets the sum
from the tarball it built.

## namcap warnings kept

`.github/ci/aur.sh` fails on any other namcap error or warning.

- `Unused shared library '/usr/lib64/ld-linux-x86-64.so.2'`: the dynamic loader every Rust binary
  names; it is glibc's.
- `Dependency included, but may not be needed` for `pipewire`, `pipewire-audio`, `wireplumber`,
  `coreutils`, `vulkan-icd-loader`, `vulkan-driver`, `libxkbcommon` and `fontconfig`: namcap sees
  only linked libraries. cardcast runs `pw-dump`, `pw-loopback` (from `pipewire-audio`),
  `wireplumber` and `timeout`, and loads Vulkan, xkbcommon and fontconfig at run time (`dlopen`),
  so without them sound, the mirror or the window fails.

The PKGBUILD follows the [PKGBUILD][pkgbuild] and [Arch package guidelines][guidelines]. The
`LICENSE` here (0BSD) covers the package files, as the [AUR submission guidelines][rules] ask.

[rules]: https://wiki.archlinux.org/title/AUR_submission_guidelines#Rules_of_submission
[auth]: https://wiki.archlinux.org/title/AUR_submission_guidelines#Authentication
[clone]: https://wiki.archlinux.org/title/AUR_submission_guidelines#Creating_package_repositories
[publish]: https://wiki.archlinux.org/title/AUR_submission_guidelines#Publishing_new_package_content
[pkgbuild]: https://wiki.archlinux.org/title/PKGBUILD
[guidelines]: https://wiki.archlinux.org/title/Arch_package_guidelines
