class Rakumux < Formula
  desc "AI-powered Git worktree and tmux session manager with modern TUI"
  homepage "https://github.com/humanunsupervised/rakumux"
  version "0.5.81"
  license "Proprietary"

  # Binary-only distribution - downloads pre-compiled packages
  if OS.mac? && Hardware::CPU.intel?
    url "https://github.com/humanunsupervised/rakumux/releases/download/v#{version}/rakumux-macos-intel.tar.gz"
    sha256 "8325b2466fcc2b43b0de77dc4f87883264069579d805c1bd438c63641341efd0"
  elsif OS.mac? && Hardware::CPU.arm?
    url "https://github.com/humanunsupervised/rakumux/releases/download/v#{version}/rakumux-macos-arm64.tar.gz"
    sha256 "358ead108682afe1b7a333d98fc95a14d0bc26f4ca3362c64cf95842bfdfb83b"
  elsif OS.linux? && Hardware::CPU.intel?
    url "https://github.com/humanunsupervised/rakumux/releases/download/v#{version}/rakumux-linux-x64.tar.gz"
    sha256 "329c8d8a47a69b215bbb9793099e0c5c110e24d3d49bd2b7a445b715e9ccb9d8"
  else
    odie "Rakumux is not available for #{OS.kernel_name} #{Hardware::CPU.arch}"
  end

  depends_on "git"
  depends_on "tmux" => :recommended
  depends_on "jq"

  def install
    # Install pre-compiled binary (renamed from tui in the package)
    bin.install "rakumux" => "rakumux-bin"
    
    # Install runtime scripts to libexec
    libexec.install "rakumux.sh"
    libexec.install "rakumux-cli.sh"
    libexec.install "services.sh"
    libexec.install "env-copy"
    libexec.install "rakumux-local.sh"
    libexec.install "shell"
    libexec.install "server"
    
    # Install API scripts
    (libexec/"api").mkpath
    (libexec/"api").install "api/git.sh"
    (libexec/"api").install "api/tmux.sh"
    
    # Create wrapper scripts that set correct paths
    (bin/"rakumux-cli").write wrapper_script("rakumux-cli.sh")
    
    # Create primary rakumux command for TUI interface
    (bin/"rakumux").write rakumux_wrapper_script()
    
    (bin/"rakumux-local").write <<~EOS
      #!/bin/bash
      exec "#{libexec}/rakumux-local.sh" "$@"
    EOS
    (bin/"rakumux-local").chmod 0555
  end

  def wrapper_script(script_name)
    <<~EOS
      #!/bin/bash
      export GW_ORCHESTRATOR_ROOT="#{libexec}"
      export GW_TUI_BIN="#{bin}/rakumux-bin"
      export GW_ENV_COPY_BIN="#{libexec}/env-copy"
      exec "#{libexec}/#{script_name}" "$@"
    EOS
  end

  def rakumux_wrapper_script
    <<~EOS
      #!/bin/bash
      # Rakumux wrapper with hanging fix
      export GW_ORCHESTRATOR_ROOT="#{libexec}"
      export GW_TUI_BIN="#{bin}/rakumux-bin"
      export GW_ENV_COPY_BIN="#{libexec}/env-copy"
      
      # Fixed wrapper logic - routes commands to avoid stdout capture hanging
      case "${1:-}" in
        ""|"--debug"|"-d"|"--help"|"-h"|"--version")
          # Interactive TUI operations - run directly to avoid stdout capture
          exec "#{libexec}/rakumux.sh" "$@"
          ;;
        *)
          # CLI operations that may need directory switching
          tmpfile="$(mktemp)"
          trap "rm -f '$tmpfile'" EXIT
          
          # Run CLI command and capture output in temp file
          "#{libexec}/rakumux-cli.sh" "$@" > "$tmpfile" 2>&1
          status=$?
          
          # Handle directory switching
          out="$(cat "$tmpfile")"
          cd_line="$(echo "$out" | grep -m1 '^cd')"
          [[ -n $cd_line ]] && eval "$cd_line"
          
          # Show output excluding cd commands  
          echo "$out" | grep -v '^cd'
          exit $status
          ;;
      esac
    EOS
  end

  def caveats
    <<~EOS
      Rakumux is ready to use! Type `rakumux` in your repo root to start.
    EOS
  end

  test do
    # Test that the binary exists and is executable
    assert_predicate bin/"rakumux-bin", :exist?
    assert_predicate bin/"rakumux-bin", :executable?
    
    # Test that wrapper scripts are accessible
    assert_predicate bin/"rakumux", :exist?
    assert_predicate bin/"rakumux-cli", :exist?
    assert_predicate bin/"rakumux-local", :exist?
    
    # Test basic help output (in a safe way)
    output = shell_output("#{bin}/rakumux-cli help 2>&1", 0)
    assert_match(/Usage|Commands|Options/, output)
  end
end
