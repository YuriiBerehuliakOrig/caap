import * as path from "path";
import * as fs from "fs";
import * as vscode from "vscode";
import { ExtensionContext, workspace, window } from "vscode";
import {
  LanguageClient,
  LanguageClientOptions,
  ServerOptions,
  TransportKind,
} from "vscode-languageclient/node";

let client: LanguageClient | undefined;
let statusBar: vscode.StatusBarItem | undefined;

export async function activate(context: ExtensionContext): Promise<void> {
  registerDebugAdapter(context);

  statusBar = vscode.window.createStatusBarItem(
    vscode.StatusBarAlignment.Right,
    100
  );
  statusBar.command = "caap.restartServer";
  context.subscriptions.push(statusBar);

  context.subscriptions.push(
    vscode.commands.registerCommand("caap.restartServer", async () => {
      await restartServer(context);
    })
  );

  await startServer(context);
}

async function startServer(context: ExtensionContext): Promise<void> {
  const command = resolveServerCommand(context);
  if (!command) {
    setStatus("CAAP: server not found", "$(error)");
    window.showErrorMessage(
      "caap-lsp binary not found. Build it with `cargo build -p caap-lsp` " +
        "or set the `caap.server.path` setting."
    );
    return;
  }

  const serverOptions: ServerOptions = {
    run: { command, transport: TransportKind.stdio },
    debug: { command, transport: TransportKind.stdio },
  };

  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: "file", language: "caap" }],
    synchronize: {
      fileEvents: workspace.createFileSystemWatcher("**/*.caap"),
    },
  };

  client = new LanguageClient(
    "caap",
    "CAAP Language Server",
    serverOptions,
    clientOptions
  );

  setStatus("CAAP: starting…", "$(sync~spin)");
  await client.start();
  setStatus("CAAP", "$(check)");
}

async function restartServer(context: ExtensionContext): Promise<void> {
  setStatus("CAAP: restarting…", "$(sync~spin)");
  if (client) {
    await client.stop();
    client = undefined;
  }
  await startServer(context);
}

function setStatus(text: string, icon: string): void {
  if (!statusBar) {
    return;
  }
  statusBar.text = `${icon} ${text}`;
  statusBar.tooltip = "Click to restart the CAAP language server";
  statusBar.show();
}

function registerDebugAdapter(context: ExtensionContext): void {
  context.subscriptions.push(
    vscode.debug.registerDebugAdapterDescriptorFactory("caap", {
      createDebugAdapterDescriptor(
        session: vscode.DebugSession
      ): vscode.ProviderResult<vscode.DebugAdapterDescriptor> {
        const override = session.configuration?.dapPath as string | undefined;
        const command = resolveDapCommand(context, override);
        if (!command) {
          window.showErrorMessage(
            "caap-dap binary not found. Build it with `cargo build -p caap-dap` " +
              "or set the `caap.dap.path` setting."
          );
          return undefined;
        }
        return new vscode.DebugAdapterExecutable(command, []);
      },
    })
  );
}

export async function deactivate(): Promise<void> {
  if (client) {
    await client.stop();
    client = undefined;
  }
  statusBar?.dispose();
  statusBar = undefined;
}

function resolveServerCommand(context: ExtensionContext): string | undefined {
  const configured = workspace
    .getConfiguration("caap")
    .get<string>("server.path");
  if (configured && configured.length > 0) {
    if (fs.existsSync(configured)) {
      return configured;
    }
    window.showWarningMessage(
      `caap.server.path = ${configured} does not exist; falling back to bundled lookups.`
    );
  }

  const workspaceRoot = workspace.workspaceFolders?.[0]?.uri.fsPath;
  const candidates: string[] = [];
  if (workspaceRoot) {
    candidates.push(
      path.join(workspaceRoot, "target", "debug", "caap-lsp"),
      path.join(workspaceRoot, "target", "release", "caap-lsp")
    );
  }
  // Sibling repo layout: when this extension is packaged inside the caap repo.
  const extRoot = context.extensionPath;
  candidates.push(
    path.join(extRoot, "..", "target", "debug", "caap-lsp"),
    path.join(extRoot, "..", "target", "release", "caap-lsp")
  );

  for (const candidate of candidates) {
    if (fs.existsSync(candidate)) {
      return candidate;
    }
  }
  // Fall back to PATH lookup.
  return "caap-lsp";
}

function resolveDapCommand(
  context: ExtensionContext,
  override?: string
): string | undefined {
  const fromLaunch = override && override.length > 0 ? override : undefined;
  const configured =
    fromLaunch ??
    workspace.getConfiguration("caap").get<string>("dap.path");
  if (configured && configured.length > 0) {
    if (fs.existsSync(configured)) {
      return configured;
    }
    window.showWarningMessage(
      `caap-dap path ${configured} does not exist; falling back to bundled lookups.`
    );
  }

  const workspaceRoot = workspace.workspaceFolders?.[0]?.uri.fsPath;
  const candidates: string[] = [];
  if (workspaceRoot) {
    candidates.push(
      path.join(workspaceRoot, "target", "debug", "caap-dap"),
      path.join(workspaceRoot, "target", "release", "caap-dap")
    );
  }
  const extRoot = context.extensionPath;
  candidates.push(
    path.join(extRoot, "..", "target", "debug", "caap-dap"),
    path.join(extRoot, "..", "target", "release", "caap-dap")
  );

  for (const candidate of candidates) {
    if (fs.existsSync(candidate)) {
      return candidate;
    }
  }
  return "caap-dap";
}
