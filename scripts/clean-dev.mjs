#!/usr/bin/env node
/**
 * 清理开发环境的残留进程。
 *
 * 背景：Tauri CLI 在 Windows 上被中断（Ctrl+C、终端关闭、进程被杀）时，
 * 它拉起的子进程（Vite dev server、cargo、应用本体）不一定随之退出，
 * 变成孤儿进程继续占用：
 *   - TCP 1420：Vite 的固定端口，被占用会让 `pnpm tauri dev` 直接失败
 *     （strictPort 是 Tauri 的要求，不能改成自动换端口）
 *   - TCP 50001+：MCP 端点监听，残留进程会让新实例换到 50002、50003……
 *
 * 本脚本只清理**属于本项目**的进程，不会误伤其他程序。
 *
 * 用法：pnpm dev:clean
 */

import { execSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import path from "node:path";

const isWindows = process.platform === "win32";
const projectRoot = path.resolve(
  path.dirname(fileURLToPath(import.meta.url)),
  "..",
);

/** Vite 固定端口（Tauri 要求）。 */
const VITE_PORT = 1420;
/** MCP 端口的扫描范围，与 Rust 侧 PORT_RANGE_START/END 一致。 */
const MCP_PORT_START = 50001;
const MCP_PORT_END = 50100;

function run(cmd) {
  try {
    return execSync(cmd, { encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] });
  } catch {
    return "";
  }
}

/** 查询监听在指定端口的进程 PID（Windows 与 Unix 分别处理）。 */
function pidsOnPort(port) {
  const pids = new Set();

  if (isWindows) {
    // netstat -ano 输出形如：TCP 127.0.0.1:1420 0.0.0.0:0 LISTENING 12345
    for (const line of run("netstat -ano").split("\n")) {
      if (!line.includes(`:${port} `) || !/LISTENING/i.test(line)) continue;
      const parts = line.trim().split(/\s+/);
      const pid = parts[parts.length - 1];
      if (/^\d+$/.test(pid) && pid !== "0") pids.add(pid);
    }
  } else {
    // lsof 可能未安装，失败时忽略。
    for (const pid of run(`lsof -ti tcp:${port}`).split("\n")) {
      if (/^\d+$/.test(pid.trim())) pids.add(pid.trim());
    }
  }

  return [...pids];
}

/** 取得进程的可执行文件路径或命令行，用于判断是否属于本项目。 */
function processInfo(pid) {
  if (isWindows) {
    const out = run(
      `powershell -NoProfile -Command "(Get-CimInstance Win32_Process -Filter \\"ProcessId=${pid}\\").CommandLine"`,
    );
    return out.trim();
  }
  return run(`ps -p ${pid} -o args=`).trim();
}

/** 结束进程。 */
function kill(pid) {
  if (isWindows) {
    run(`taskkill /F /PID ${pid}`);
  } else {
    run(`kill -9 ${pid}`);
  }
}

/**
 * 该进程是否明确属于本项目。
 *
 * 判定标准刻意保守：只有命令行里出现本项目目录，才认为可以安全终止。
 * 之所以不能用「含 vite/pnpm 字样」这种宽松条件——若用户另有项目的
 * Vite 恰好占用同一端口，宽松判定会误杀别人的开发服务器。
 */
function belongsToProject(info) {
  const normalized = (info || "").replace(/\\/g, "/").toLowerCase();
  const root = projectRoot.replace(/\\/g, "/").toLowerCase();
  return normalized.includes(root);
}

function main() {
  console.log(`项目目录：${projectRoot}`);
  console.log(`清理平台：${isWindows ? "Windows" : process.platform}\n`);

  const targets = new Map(); // pid -> 原因

  // 1) Vite 端口
  for (const pid of pidsOnPort(VITE_PORT)) {
    targets.set(pid, `占用 Vite 端口 ${VITE_PORT}`);
  }

  // 2) MCP 端口（残留实例会一直向后找端口，故扫描一段范围）
  for (let port = MCP_PORT_START; port <= MCP_PORT_END; port++) {
    for (const pid of pidsOnPort(port)) {
      targets.set(pid, `占用 MCP 端口 ${port}`);
    }
  }

  if (targets.size === 0) {
    console.log("没有发现占用端口的残留进程。\n");
  }

  let killed = 0;
  const skipped = [];

  for (const [pid, reason] of targets) {
    const info = processInfo(pid);

    if (!belongsToProject(info)) {
      // 不属于本项目：只提示，绝不擅自终止（可能是用户的其他程序）。
      skipped.push({ pid, reason, info });
      continue;
    }

    console.log(`结束 PID ${pid}（${reason}）`);
    kill(pid);
    killed++;
  }

  // 3) 残余的 mf-perch 应用进程（可能已释放端口但窗口仍在）
  if (isWindows) {
    const out = run('tasklist /FI "IMAGENAME eq mf-perch.exe" /NH');
    for (const line of out.split("\n")) {
      const m = line.match(/^mf-perch\.exe\s+(\d+)/i);
      if (!m) continue;
      const pid = m[1];
      if (targets.has(pid)) continue;
      console.log(`结束 PID ${pid}（残留的 mf-perch 应用进程）`);
      kill(pid);
      killed++;
    }
  }

  console.log(
    `完成：结束 ${killed} 个进程${skipped.length > 0 ? `，跳过 ${skipped.length} 个` : ""}。`,
  );

  // 无法确认归属的进程需人工判断，避免误杀其他项目。
  if (skipped.length > 0) {
    console.log("\n以下进程占用了相关端口，但无法确认属于本项目，已跳过：");
    for (const s of skipped) {
      console.log(`  PID ${s.pid}（${s.reason}）`);
      console.log(`    命令行：${(s.info || "(无法读取)").slice(0, 140)}`);
    }
    console.log(
      "\n若确认这些进程属于本项目的残留，请手动结束对应 PID 后再重试；\n" +
        "若它们属于其他程序，请关闭该程序或改用其他端口。",
    );
  } else if (killed > 0) {
    console.log("现在可以重新运行：pnpm tauri dev");
  }
}

main();
