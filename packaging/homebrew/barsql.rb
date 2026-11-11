# Bare7a/BarSQL's release workflow writes this from its packaging/homebrew/barsql.rb, with the version and digests.
cask "barsql" do
  arch arm: "arm64", intel: "amd64"

  version "@VERSION@"
  sha256 arm:   "@SHA256_ARM64@",
         intel: "@SHA256_AMD64@"

  url "https://github.com/Bare7a/BarSQL/releases/download/v#{version}/BarSQL-darwin-#{arch}.zip"
  name "BarSQL"
  desc "SQL client for PostgreSQL, MySQL, SQL Server, ClickHouse, SQLite and Turso"
  homepage "https://barsql.bare7a.eu/"

  auto_updates true
  depends_on macos: :monterey

  app "BarSQL.app"

  uninstall quit: "eu.bare7a.barsql"

  zap trash: [
    "#{appdir}/BarSQL-data",
    "~/Library/Application Support/BarSQL-data",
    "~/Library/Preferences/eu.bare7a.barsql.plist",
  ]
end
