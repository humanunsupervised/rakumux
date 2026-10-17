import type { Metadata, Viewport } from "next";
import { GeistSans } from "geist/font/sans";
import { GeistMono } from "geist/font/mono";
import "./globals.css";

const title = "Orchestra: every coding agent, one terminal";
const description =
  "Orchestra is a terminal session manager for developers who run coding agents. Claude Code, Codex, OpenCode, dev servers and shells keep running in the background and show up live, side by side, one key away.";

export const metadata: Metadata = {
  title,
  description,
  applicationName: "Orchestra",
  keywords: ["terminal", "tui", "coding agents", "claude code", "codex", "opencode", "mcp", "session manager", "tmux alternative"],
  openGraph: { title, description, type: "website", siteName: "Orchestra" },
  twitter: { card: "summary", title, description },
};

export const viewport: Viewport = {
  themeColor: "#101119",
  colorScheme: "dark",
};

export default function RootLayout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en" className={`${GeistSans.variable} ${GeistMono.variable}`}>
      <body>{children}</body>
    </html>
  );
}
