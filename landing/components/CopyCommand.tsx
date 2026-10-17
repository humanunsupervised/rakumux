"use client";

import { useState } from "react";

/** A shell command with a copy button. */
export default function CopyCommand({ command, prompt = "$" }: { command: string; prompt?: string }) {
  const [copied, setCopied] = useState(false);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(command);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1600);
    } catch {
      // Clipboard blocked (e.g. an insecure context): the text stays selectable.
    }
  };

  return (
    <div className="command">
      <code>
        <span className="command-prompt" aria-hidden="true">
          {prompt}
        </span>
        {command}
      </code>
      <button type="button" className="command-copy" onClick={copy} aria-label={`Copy: ${command}`}>
        {copied ? "Copied" : "Copy"}
      </button>
    </div>
  );
}
