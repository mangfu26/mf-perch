# cargo 构建：坏产物（E0463 / E0462 / E0460）、并行度与编译耗时

> 状态：现行
> 构建在没有任何代码错误时失败（`.rlib` 缺失 / 页面文件耗尽）、或嫌编译慢时，先判断瓶颈在内存还是磁盘，再决定 `-j` 取多大。

---

## 首次 `pnpm tauri dev` 编译很慢（2–3 分钟）

正常现象：Rust 侧依赖（`russh`、`rusqlite`/SQLite、`rmcp` 等）首次需要全量编译。
之后增量编译通常只占几秒到几十秒。若修改了 `Cargo.toml` 的依赖，会重新触发较长编译。

---

## `cargo build` / `cargo test` 报 `E0463` / `E0462` / `E0460`：`.rlib` 缺失、crate 无法加载

### 现象

构建在**没有任何代码错误**的情况下失败，报错彼此矛盾且都指向依赖：

```
error[E0463]: can't find crate for `mf_perch_lib`
error: crate `tokio` required to be available in rlib format, but was not found in this form
error[E0460]: found possibly newer version of crate `webview2_com_sys` which `mf_perch_lib` depends on
error[E0786]: found invalid metadata files for crate `webview2_com_sys`
  = note: failed to mmap file '...\libwebview2_com_sys-....rlib': 页面文件太小，无法完成操作。 (os error 1455)
```

特征：`target/debug/deps` 里 `.d` 文件齐全，但对应的 `.rlib` / `.rmeta` **缺失或只有 0 字节**；
cargo 的指纹仍认为"已是最新"，于是不去重建，直到链接阶段才暴露产物是坏的。

### 原因

**Windows 页面文件（虚拟内存）耗尽**。最后那行 `os error 1455`（"页面文件太小"）才是根因：
rustc 写 `.rlib` 失败，留下不完整的产物与"最新"的指纹记录。上面的 `E0460`
（"possibly newer version"）是**症状而不是病因**，不要去查依赖版本冲突。

本项目特别容易触发，因为：

1. Rust 侧依赖重（`tauri`、`russh`、`rusqlite` bundled SQLite、`rmcp`），默认并发会同时跑
   N 个 rustc（N = CPU 核数），内存峰值很高；
2. `src-tauri/target` 是**共享资源**——同时开两个构建（两个 Agent / 两个终端，
   或一个 `pnpm tauri dev` 加一个 `cargo test`）会成倍叠加内存压力。
   cargo 的文件锁只能让构建**串行**，防不住单个构建自身的内存峰值。

### 解决办法

```bash
cd src-tauri
cargo clean          # 必须先清：坏产物的指纹是"最新"，不清就不会重建
cargo test -j 2      # 限制并发，避免再次耗尽页面文件
```

`target` 约 11 GB（2026-09-18 `du -sh` 实测），`cargo clean` 后首次全量编译需数分钟到十几分钟；之后增量编译恢复正常。

### 预防

- **并发跑构建时一律加 `-j 2`**（或设 `CARGO_BUILD_JOBS=2`）；
- 不要在 `pnpm tauri dev` 还在编译时另开 `cargo test` / `cargo build`；
- 看到 `E0460` / `E0463` 先往下翻有没有 `os error 1455`，有就直接走上面的恢复步骤，
  不要改 `Cargo.toml`；
- **为什么不干脆把并行度调大、换回更快的构建？** 见下一节：在内存不足、`target` 又在慢盘上时，
  调大不是更快，只是把瓶颈从 CPU 换成内存与磁盘。

---

## 为什么不把 `cargo` 的并行度调大

### 结论：按条件判断，不是按某台机器判断

**同时**满足下面两条时，提高 `-j` 换不来速度，只是把瓶颈从 CPU 搬到内存与磁盘，
并且会触发上一节的坏产物：

1. **空闲内存不足以容纳 `并发数 × 单个 rustc 峰值`**——Rust 重依赖工程里，
   单个 `rustc` 在优化 / 链接前后峰值可达数百 MB 至 GB 级；
2. **`target` 落在随机 I/O 慢的介质上**——机械盘，或被拥塞的网络盘。

本项目当前的开发机两条都满足，因此**一律 `-j 2`**。
不满足的机器（内存充裕且 `target` 在 NVMe/SSD，或 CI）可以调大，但**先测再改**
（判据见下面"怎么判断自己的机器属于哪种"）。

### 机制

1. **CPU 通常不是瓶颈**：`cargo` 的默认并发取**逻辑核数**。若 `-j 2` 下缓存命中的全量
   测试只要几十秒，说明时间主要花在 I/O 上而非算力——此时调大 `-j` 没有收益来源；
2. **内存是硬边界**：超出物理内存的部分只能进页面文件，而换页本身就意味着内存已不够；
3. **磁盘随机 I/O 才是真正的瓶颈**：多个 `rustc` 同时读写上万个小文件 → 寻道排队。
   即使页面文件在 SSD 上，被换出的编译进程再次被调度时仍要等 I/O。

三者叠加时，并行度越高**总耗时反而可能更长**，还附带 E0463 风险。

### 参考实测（2026-09-16，一台同时满足上述两条的开发机）

> 这些数字用来**支撑上面的机制判断**，不是规则的适用前提；换机器请按同样口径重测。

| 项 | 实测值 | 取法（PowerShell） |
| ---- | ---- | ---- |
| CPU | Intel i5-8250U，**4 物理核 / 8 逻辑核** | `Get-CimInstance Win32_Processor` |
| 内存 | 总 **7.89 GB**，空闲 **2.28 GB** | `Win32_OperatingSystem` 的 `TotalVisibleMemorySize` / `FreePhysicalMemory` |
| 页面文件 | `C:\pagefile.sys` 分配 **13.5 GB**，**峰值用量 4.75 GB** | `Win32_PageFileUsage` 的 `PeakUsage` |
| `src-tauri/target` | **12.33 GB / 10745 个文件**，位于 D 盘 | `Get-ChildItem -Recurse \| Measure-Object` |
| D 盘 | **TOSHIBA MQ01ABF050，SATA 机械盘**（`target` 在这里） | `Get-Partition -DriveLetter D \| Get-Disk` |
| C 盘 | Maxsun 120GB A6，SSD（页面文件在这里） | `Get-Partition -DriveLetter C \| Get-Disk` |
| `sccache` | **未安装** | `Get-Command sccache` |

> 该机的页面文件峰值 4.75 GB 不是空谈：E0463 的成因就是它被耗尽（`os error 1455`）。

### 怎么判断自己的机器属于哪种

- **磁盘活动时间长期 100%** → 瓶颈是 I/O，调并行度帮不上（条件 2 成立）；
- **CPU 长期满载而磁盘空闲** → 瓶颈才是算力，调大 `-j` 有意义；
- **空闲内存 < 期望并发数 × 1 GB** → 条件 1 成立，先别调大；
- 对照实验比观察更可靠：`cargo clean -p mf-perch` 后分别用 `-j 2` 与更大值各跑一次，
  比**总耗时**——不要只看单个 crate 的编译速度。

### 为什么不引入 `sccache`

`sccache` 缓存的是**编译单元产物**，而上面两个条件同时成立时最慢的环节是**链接**与
**磁盘随机 I/O**——这两样它都缓存不了。它还要额外安装二进制、维护缓存目录，并引入一类
新故障：缓存损坏或误命中会制造"改了代码但结果没变"——**正是 AGENTS.md §5.11 纪律 2
要防的那个假象**。增量编译本来已够快（参考实测：缓存命中 20–50 秒）时，
**收益不确定、故障面清楚**，不划算。（客户 2026-09-16 已确认：维持原样。）

### 何时重新评估

- `target` 迁到 SSD / NVMe，或空闲内存达到 `期望并发数 × 单个 rustc 峰值` 以上；
- 换到明显更快的构建机 / CI；
- **缓存命中的情况下**单次全量编译明显超过 1 分钟——先查是不是别的进程在抢磁盘，再谈 `-j`。

### 不要做的事

- **不要**为了"跑快点"把 `-j` 调大：收益要先按上面的对照实验测出来，
  代价却是确定的坏产物 + `cargo clean`（清完重编十几分钟）；
- **不要**把 `target` 放内存盘：`target` 是 GB 级（参考实测 12.33 GB），
  通常比开发机的空闲内存还大；
- **不要**用 `sccache` 掩盖"编译慢"的症状：先按"怎么判断"一节确认瓶颈在哪。
