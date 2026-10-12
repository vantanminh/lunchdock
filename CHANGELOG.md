# Changelog

What changes in each Rudder release, written for the people who run it. **System → Updates** in the panel shows the entries of every release it is about to install, so each line should say what a person will notice, not which files moved.

Every change that someone can see or that changes how Rudder behaves gets a line here in the same pull request. When a release is cut, `[Unreleased]` is renamed to the version and date and a new empty `[Unreleased]` is added on top. The release workflow publishes the section of the tag as the release notes and refuses to run when it is empty.

Sections: **Added** (new things), **Changed** (existing behavior), **Fixed**, **Security**, **Removed**. Earlier releases are listed on [GitHub Releases](https://github.com/vantanminh/lunchdock/releases).

## [Unreleased]

### Added
- **Connect GitHub with one button.** Git providers has a "Connect with GitHub" card that creates and installs a private GitHub App for you; there is no app id, installation id or private key to copy any more. Pushes to the repositories you pick build the matching services when the panel can be reached from the internet.
- **Release notes for everything an update brings in.** System → Updates now lists the notes of every release between the installed version and the newest one, and shows them before you press Update.

## [0.1.5] - 2026-10-12

### Changed
- **Projects page redesigned** as a workspace: projects on the left, the selected project's environments and services on the right, with search and a phone layout.
- **Service page redesigned** with clearer tabs and details.
