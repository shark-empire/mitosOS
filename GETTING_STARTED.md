# Getting this from a zip into a working GitHub repo (mobile)

1. Extract the zip — Readdle Documents can do this (tap the zip, choose
   Extract).
2. On github.com, create a new **empty** repository (skip the README/
   .gitignore/license options, so nothing conflicts with the upload).
3. Open the new repo, tap **Add file → Upload files**, then select/drag in
   the *contents* of the extracted `mitosos/` folder — not the folder
   itself. GitHub's upload UI preserves nested folder paths.
4. Commit straight to `main`.
5. **Check the hidden folders made it**: open the repo and confirm
   `.github/workflows/ci.yml` and `.cargo/config.toml` are actually
   there. Mobile browsers and some upload flows occasionally drop
   dotfiles. If either is missing:
   - Tap **Add file → Create new file**.
   - Type the *full path* as the filename, e.g.
     `.github/workflows/ci.yml` — GitHub auto-creates the folders for you.
   - Paste in the contents from this project and commit.
6. Go to the **Actions** tab. A `kernel-boot-test` run should start
   automatically — that's your kernel building and booting in QEMU on
   GitHub's servers. Green check = it booted and printed the expected
   line. Red X = open the log and send it to me, we'll debug it together.

From here on, every push gets boot-tested automatically — that's your
test rig, since you're building this entirely from mobile. Getting it
onto real Pi hardware (see README.md) is the one step that needs an
actual desktop with an SD card reader — there's no way around that part.
