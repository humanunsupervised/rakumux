"use client";

import { useState } from "react";
import CopyCommand from "./CopyCommand";

const RELEASES = "https://github.com/humanunsupervised/rakumux/releases";

const TARBALLS = [
  { label: "macOS · Apple Silicon", file: "rakumux-macos-arm64" },
  { label: "macOS · Intel", file: "rakumux-macos-intel" },
  { label: "Linux · x64", file: "rakumux-linux-x64" },
];

type Tab = "npm" | "brew" | "download";

export default function InstallTabs() {
  const [tab, setTab] = useState<Tab>("npm");
  const [platform, setPlatform] = useState(0);
  const file = TARBALLS[platform].file;

  const tabs: { id: Tab; label: string }[] = [
    { id: "npm", label: "npm" },
    { id: "brew", label: "Homebrew" },
    { id: "download", label: "Download" },
  ];

  return (
    <div className="install">
      <div className="tabs" role="tablist" aria-label="Install method">
        {tabs.map((t) => (
          <button
            key={t.id}
            type="button"
            role="tab"
            id={`tab-${t.id}`}
            aria-selected={tab === t.id}
            aria-controls={`panel-${t.id}`}
            className={tab === t.id ? "is-active" : undefined}
            onClick={() => setTab(t.id)}
          >
            {t.label}
          </button>
        ))}
      </div>

      <div className="install-panel" role="tabpanel" id={`panel-${tab}`} aria-labelledby={`tab-${tab}`}>
        {tab === "npm" && (
          <>
            <CopyCommand command="npm install -g rakumux" />
            <p className="install-note">Node.js 14 or newer. The package carries prebuilt binaries for every platform.</p>
          </>
        )}
        {tab === "brew" && (
          <>
            <CopyCommand command="brew install humanunsupervised/rakumux/rakumux" />
            <p className="install-note">
              macOS and Linux. Update with <code>brew upgrade rakumux</code>.
            </p>
          </>
        )}
        {tab === "download" && (
          <>
            <div className="platforms" role="radiogroup" aria-label="Platform">
              {TARBALLS.map((t, i) => (
                <button
                  key={t.file}
                  type="button"
                  role="radio"
                  aria-checked={platform === i}
                  className={platform === i ? "is-active" : undefined}
                  onClick={() => setPlatform(i)}
                >
                  {t.label}
                </button>
              ))}
            </div>
            <CopyCommand command={`curl -L ${RELEASES}/latest/download/${file}.tar.gz | tar xz`} />
            <CopyCommand command={`cd ${file} && ./install.sh`} />
            <p className="install-note">
              Installs to <code>/usr/local</code>; set <code>PREFIX=&quot;$HOME/.local&quot;</code> for another prefix.
              Checksums are on the <a href={RELEASES}>release page</a>.
            </p>
          </>
        )}
      </div>
    </div>
  );
}
