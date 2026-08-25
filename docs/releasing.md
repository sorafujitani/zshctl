# Releasing zshctl

zshctl releases are immutable SemVer tags. The workspace version, Git tag,
GitHub Release, binary archive names, and Homebrew Formula version must agree.

## Publish a release

1. Update `version` in the workspace package metadata and refresh `Cargo.lock`.
2. Run the checks documented in the repository README.
3. Commit and push the release-ready revision to `main`.
4. Create and push an annotated `vMAJOR.MINOR.PATCH` tag:

   ```sh
   git tag -a v0.1.0 -m "zshctl v0.1.0"
   git push origin v0.1.0
   ```

5. Wait for the `Release artifacts` workflow. It rejects a tag that does not
   match the Cargo workspace version, verifies the tagged source, and publishes
   checksummed archives for supported macOS and Linux targets.
6. Point `Formula/zshctl.rb` in `sorafujitani/homebrew-tap` at the immutable tag
   archive, update its SHA-256, and run the Formula audit, test, and installation
   checks before pushing the tap change.

Never move or reuse a published version tag. If a release is wrong, increment
the patch version and publish a new tag.
