# 3-Way Merge 功能验收说明

## 最快速的验收方法

### 1. 运行测试脚本创建冲突环境

```bash
./scripts/create_merge_test.sh
```

这个脚本会：
- ✅ 创建一个新的 Git 测试仓库
- ✅ 自动生成两个分支的不同修改
- ✅ 触发合并冲突
- ✅ 启用 diff3 格式（显示 base 版本）
- ✅ 在 macOS 上自动打开测试目录

### 2. 在 Zed 中验证

```bash
# 构建并运行 Zed
cargo run --release

# 或者如果已经构建过
./target/release/zed
```

在 Zed 中：
1. 打开脚本输出的测试目录
2. 打开 `README.md` 文件
3. **查看冲突区域上方是否出现三栏面板**

## 期望看到的效果

```
┌─────────────────────────────────────────────────────────────────┐
│  Base (Common Ancestor)  │  Ours (main)  │  Theirs (feature)   │
├─────────────────────────────────────────────────────────────────┤
│ ## Section 1             │ ## Section 1  │ ## Section 1        │
│ Original content...      │ **MAIN        │ **FEATURE           │
│                          │ BRANCH        │ BRANCH              │
│                          │ CHANGE**...   │ CHANGE**...         │
├─────────────────────────────────────────────────────────────────┤
│     [Accept Base]  [Accept main]  [Accept feature]  [Accept Both]│
└─────────────────────────────────────────────────────────────────┘
```

## 关键验收点

- [ ] **三栏布局正确显示** - 看到三个独立的文本区域
- [ ] **Base 栏显示原始内容** - "Original content in section 1"
- [ ] **Ours 栏显示主分支修改** - "MAIN BRANCH CHANGE"
- [ ] **Theirs 栏显示 feature 分支修改** - "FEATURE BRANCH CHANGE"
- [ ] **四个按钮都可点击** - Accept Base/Ours/Theirs/Both
- [ ] **点击按钮后冲突标记消失** - 只保留选中的版本

## 如果看不到 3-Way UI

可能的原因：
1. **Git 没有启用 diff3** - 脚本会自动启用，但可以手动检查：
   ```bash
   git config merge.conflictstyle
   # 应该输出: diff3
   ```

2. **冲突没有 base 部分** - 检查冲突标记是否包含 `||||||| base`

3. **代码未正确编译** - 重新编译：
   ```bash
   cargo build --release --package zed
   ```

## 代码层面的验收

如果你想验证代码实现：

```bash
# 1. 检查关键文件是否存在
test -f crates/git_ui/src/three_way_merge_view.rs && echo "✅ 3-way merge 组件存在" || echo "❌ 组件缺失"

# 2. 检查 Git 接口是否扩展
grep -q "load_merge_stage_text" crates/git/src/repository.rs && echo "✅ Git 接口已扩展" || echo "❌ 接口未扩展"

# 3. 检查数据模型是否增强
grep -q "base_text: Option<String>" crates/project/src/git_store/conflict_set.rs && echo "✅ 数据模型已增强" || echo "❌ 数据模型未修改"

# 4. 检查 UI 集成
grep -q "ThreeWayMergeView" crates/git_ui/src/conflict_view.rs && echo "✅ UI 已集成" || echo "❌ UI 未集成"

# 5. 确保编译通过
cargo check --package git_ui && echo "✅ git_ui 编译通过" || echo "❌ 编译失败"
```

## 预期输出示例

所有检查应该输出 ✅：
```
✅ 3-way merge 组件存在
✅ Git 接口已扩展
✅ 数据模型已增强
✅ UI 已集成
✅ git_ui 编译通过
```

## 手动测试步骤（详细版）

如果自动脚本有问题，可以手动创建：

```bash
# 1. 创建测试目录
mkdir /tmp/manual-merge-test
cd /tmp/manual-merge-test

# 2. 初始化并配置
git init
git config merge.conflictstyle diff3

# 3. 创建并提交初始文件
echo -e "line 1\nline 2\nline 3" > test.txt
git add test.txt
git commit -m "initial"

# 4. 创建并修改 feature 分支
git checkout -b feature
echo -e "line 1\nfeature change\nline 3" > test.txt
git commit -am "feature"

# 5. 回到 main 并做不同修改
git checkout main
echo -e "line 1\nmain change\nline 3" > test.txt
git commit -am "main"

# 6. 合并产生冲突
git merge feature  # 会失败并产生冲突

# 7. 在 Zed 中打开 test.txt
```

## 问题排查

如果遇到问题：

1. **查看编译日志**
   ```bash
   cargo build --package zed 2>&1 | tee build.log
   ```

2. **检查 Zed 运行日志**
   ```bash
   # Zed 的日志位置
   tail -f ~/Library/Logs/Zed/Zed.log
   ```

3. **验证 Git 配置**
   ```bash
   cd /tmp/zed-merge-test-*
   git config --list | grep merge
   cat README.md  # 查看冲突标记格式
   ```

## 成功标准

验收通过的标志：
- ✅ 能看到三栏布局的冲突解决界面
- ✅ 每栏显示正确的文本内容
- ✅ 按钮操作能正确解决冲突
- ✅ 没有崩溃或错误信息
- ✅ 用户体验流畅自然

## 联系我

如果验收过程中有任何问题，请提供：
- 截图或录屏
- 错误日志
- Git 仓库状态
- Zed 版本信息
