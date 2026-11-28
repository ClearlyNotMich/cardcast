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

3. Tag a release (say `v0.2.0`) with `pkgver` in the PKGBUILD equal to Cargo.toml's version (CI
   checks this), and wait for the Release workflow to attach `cardcast-v0.2.0-linux-x86_64.tar.gz`.
4. Clone the empty package and copy the files in ([Creating package repositories][clone]):

   ```
   git -c init.defaultBranch=master clone ssh://aur@aur.archlinux.org/cardcast-bin.git
   cp packaging/aur/cardcast-bin/{PKGBUILD,.SRCINFO,LICENSE} cardcast-bin/
   cd cardcast-bin
   ```

5. Set the checksum and push (as in "Every release" below, steps 2 to 4).

## Every release

1. In `cardcast-bin/PKGBUILD` set `pkgver` to the tag without the `v`, and `pkgrel=1`. A PKGBUILD
   fix with no new release raises `pkgrel` instead ([Publishing][publish]).
2. `updpkgsums` (from `pacman-contrib`) downloads the release tarball and writes its sum into
   `sha256sums_x86_64`; it must match the release's `.sha256` file.
3. `makepkg -si` to build and install it, then `makepkg --printsrcinfo > .SRCINFO`.
4. `git add PKGBUILD .SRCINFO LICENSE && git commit -m "Update to <version>" && git push` (the AUR
   takes pushes to `master` only).
5. Copy `PKGBUILD` and `.SRCINFO` back into this repo's `packaging/aur/cardcast-bin/`.

The committed `sha256sums_x86_64=('SKIP')` stands until step 2 of the first push; CI sets the sum
from the tarball it built.

## namcap warnings kept

`.github/ci/aur.sh` fails on any other namcap error or warning.

- `Unused shared library '/usr/lib64/ld-linux-x86-64.so.2'`: the dynamic loader every Rust binary
  names; it is glibc's.
- `Dependency included, but may not be needed` for `pipewire`, `pipewire-audio`, `wireplumber`,
  `coreutils`, `vulkan-icd-loader`, `vulkan-driver`, `libxkbcommon` and `fontconfig`: namcap sees
  only linked libraries. cardcast runs `pw-dump`, `pw-loopback`, `wireplumber` and `timeout`, and
  loads Vulkan, xkbcommon and fontconfig at run time (`dlopen`), so without them sound, the mirror
  or the window fails.

The PKGBUILD follows the [PKGBUILD][pkgbuild] and [Arch package guidelines][guidelines]. The
`LICENSE` here (0BSD) covers the package files, as the [AUR submission guidelines][rules] ask.

[rules]: https://wiki.archlinux.org/title/AUR_submission_guidelines#Rules_of_submission
[auth]: https://wiki.archlinux.org/title/AUR_submission_guidelines#Authentication
[clone]: https://wiki.archlinux.org/title/AUR_submission_guidelines#Creating_package_repositories
[publish]: https://wiki.archlinux.org/title/AUR_submission_guidelines#Publishing_new_package_content
[pkgbuild]: https://wiki.archlinux.org/title/PKGBUILD
[guidelines]: https://wiki.archlinux.org/title/Arch_package_guidelines
