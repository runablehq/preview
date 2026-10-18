# Releasing

```text
merge the release PR → tag vX.Y.Z → build 4 targets → GitHub release
  → npm publish (dist-tag next) → smoke matrix installs from npm → dist-tag latest
```

1. Merge to `main` with conventional commits (`feat:`, `fix:`, …). `release-please.yml` keeps a
   release PR open that bumps `Cargo.toml`, `Cargo.lock`, `npm/*/package.json` and
   `CHANGELOG.md`.
2. Merging that PR tags `vX.Y.Z` and creates the GitHub release. The tag starts `release.yml`.
3. `release.yml` builds, uploads the tarballs, `SHA256SUMS` and `install.sh` to the release, and
   publishes the five npm packages with dist-tag `next`. Next it installs from npm on four
   runners and runs `tests/smoke.sh`. When those pass, it moves `latest` to the new version.

`scripts/pack-npm.sh` writes the version from the tag into every package and pins the main
package's `optionalDependencies` to it, so the files in `npm/` only need the right shape.

To rerun a failed release, use *Run workflow* on `release.yml` with the tag. Packages that are
already on npm are skipped.

## One-time setup

- **Release PR token.** A tag pushed with the default `GITHUB_TOKEN` does not start other
  workflows. Add `RELEASE_PLEASE_TOKEN`, a GitHub App or fine-grained token with contents and
  pull-requests write.
- **`release` environment** with a required reviewer. `npm-publish` and `promote` run in it.
- **npm packages.** Trusted publishing is set on a package that already exists. So publish
  0.1.0 of each package once by hand: `scripts/pack-npm.sh 0.1.0 <tarballs> out`, then
  `npm publish --access public` in each folder. After that, add a trusted publisher on
  npmjs.com for each of the five packages: repository `runablehq/preview`, workflow
  `release.yml`, environment `release`. Allow both `npm publish` and `npm dist-tag`.
- **Tokenless npm publishing and promotion.** Both jobs use GitHub Actions OIDC with
  `id-token: write`. Promotion uses npm 12.2.0 or newer for OIDC dist-tag support, so no
  `NPM_TOKEN` secret is needed.

Registry scanning can delay a newly published version and its platform packages independently.
The smoke jobs retry for about 15 minutes and require the installed native binary to report the
expected version before proceeding.

## Rolling back

Deprecate the bad version and point `latest` at the good one, for all five packages:

```sh
for p in preview preview-linux-x64 preview-linux-arm64 preview-darwin-x64 preview-darwin-arm64; do
  npm deprecate "@runablehq/$p@<bad version>" "<reason>"
  npm dist-tag add "@runablehq/$p@<good version>" latest
done
```

New installs get the good version right away. Machines that already run the bad one switch at
their next update check, within about 6 hours (`preview upgrade` does it at once). Updaters
read the platform packages, so the deprecation must be on those, not only on the main one.
