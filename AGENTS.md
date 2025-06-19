## Use gitmoji for every git commit.

## AI testing responsibilities

- The AI is responsible for all tests.
- Add, remove, or modify tests as needed when making code changes.
- Always run the full test suite after updates.

### Specification

You can extend Gitmoji and make it your own, but in case you want to follow the official specification, please continue reading 👀

A gitmoji commit message consists is composed using the following pieces:

    intention: The intention you want to express with the commit, using an emoji from the list. Either in the :shortcode: or unicode format.
    scope: An optional string that adds contextual information for the scope of the change.
    message: A brief explanation of the change.

<intention> [scope?][:?] <message>

Examples

    ⚡️ Lazyload home screen images.
    🐛 Fix `onClick` event handler
    🔖 Bump version `1.2.0`
    ♻️ (components): Transform classes to hooks
    📈 Add analytics to the dashboard
    🌐 Support Japanese language
    ♿️ (account): Improve modals a11y

List of Gitmojis and their meaning:

    * 🎨  Improve structure / format of the code
    * ⚡  Improve performance
    * 🔥  Remove code or files
    * 🐛  Fix a bug
    * 🚑  Critical hotfix
    * ✨  Introduce new features
    * 📝  Add or update documentation
    * 🚀  Deploy stuff
    * 💄  Add or update the UI and style files
    * 🎉  Begin a project
    * ✅  Add, update, or pass tests
    * 🔒  Fix security or privacy issues
    * 🔐  Add or update secrets
    * 🔖  Release / version tags
    * 🚨  Fix compiler / linter warnings
    * 🚧  Work in progress
    * 💚  Fix CI build
    * ⬇️  Downgrade dependencies
    * ⬆️  Upgrade dependencies
    * 📌  Pin dependencies to specific versions
    * 👷  Add or update CI build system
    * 📈  Add or update analytics or track code
    * ♻️  Refactor code
    * ➕  Add a dependency
    * ➖  Remove a dependency
    * 🔧  Add or update configuration files
    * 🔨  Add or update development scripts
    * 🌐  Internationalization and localization
    * ✏️  Fix typos
    * 💩  Write bad code that needs to be improved
    * ⏪  Revert changes
    * 🔀  Merge branches
    * 📦  Add or update compiled files or packages
    * 👽  Update code due to external API changes
    * 🚚  Move or rename resources (e.g., files, paths, routes)
    * 📄  Add or update license
    * 💥  Introduce breaking changes
    * 🍱  Add or update assets
    * ♿  Improve accessibility
    * 💡  Add or update comments in source code
    * 🍻  Write code drunkenly
    * 💬  Add or update text and literals
    * 🗃  Perform database‑related changes
    * 🔊  Add or update logs
    * 🔇  Remove logs
    * 👥  Add or update contributor(s)
    * 🚸  Improve user experience / usability
    * 🏗  Make architectural changes
    * 📱  Work on responsive design
    * 🤡  Mock things
    * 🥚  Add or update an easter egg
    * 🙈  Add or update a .gitignore file
    * 📸  Add or update snapshots
    * ⚗️  Perform experiments
    * 🔍  Improve SEO
    * 🏷️  Add or update types
    * 🌱  Add or update seed files
    * 🚩  Add, update, or remove feature flags
    * 🥅  Catch errors
    * 💫  Add or update animations and transitions
    * 🗑️  Deprecate code that needs to be cleaned up
    * 🛂  Work on code related to authorization, roles, and permissions
    * 🩹  Simple fix for a non‑critical issue
    * 🧐  Data exploration / inspection
    * ⚰️  Remove dead code
    * 🧪  Add a failing test
    * 👔  Add or update business logic
    * 🩺  Add or update healthcheck
    * 🧱  Infrastructure‑related changes
    * 🧑‍💻  Improve developer experience
    * 💸  Add sponsorships or money‑related infrastructure
    * 🧵  Add or update code related to multithreading or concurrency
    * 🦺  Add or update code related to validation
    * ✈️  Improve offline support
