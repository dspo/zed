# Zed 3-Way Merge 功能验收指南

## 验收方式

### 方式 1: 创建测试冲突（推荐）

#### 步骤：

1. **编译并运行 Zed**
```bash
cd /Users/chenzhongrun/projects/gpui-project/zed
cargo run --release
```

2. **创建测试仓库**
```bash
# 创建一个新的测试目录
mkdir /tmp/merge-test
cd /tmp/merge-test
git init

# 创建初始文件
cat > test.txt << 'EOF'
Line 1: Original content
Line 2: Original content
Line 3: Original content
Line 4: Original content
Line 5: Original content
EOF

git add test.txt
git commit -m "Initial commit"

# 创建并切换到分支 feature
git checkout -b feature
cat > test.txt << 'EOF'
Line 1: Original content
Line 2: Feature branch change
Line 3: Feature branch addition
Line 4: Original content
Line 5: Original content
EOF
git commit -am "Feature changes"

# 切换回 main 并做不同的修改
git checkout main
cat > test.txt << 'EOF'
Line 1: Original content
Line 2: Main branch change
Line 3: Original content
Line 4: Main branch addition
Line 5: Original content
EOF
git commit -am "Main changes"

# 尝试合并 - 这会产生冲突
git merge feature
```

3. **在 Zed 中打开冲突文件**
   - 用 Zed 打开 `/tmp/merge-test` 文件夹
   - 打开 `test.txt` 文件
   - 你应该看到冲突标记

4. **验证 3-Way Merge UI**

**期望看到：**
- ✅ 在冲突区域上方出现一个三栏面板
- ✅ 左栏显示 "Base (Common Ancestor)" - 显示原始版本
- ✅ 中栏显示 "Ours (main)" - 显示当前分支的版本
- ✅ 右栏显示 "Theirs (feature)" - 显示要合并的分支版本
- ✅ 每栏有不同的背景色
- ✅ 底部有四个按钮：
  - "Accept Base"
  - "Accept main" (或你的分支名)
  - "Accept feature" (或对方分支名)
  - "Accept Both"

5. **测试合并操作**
   - 点击任一按钮
   - 验证冲突标记被正确移除
   - 验证选择的版本被保留

---

### 方式 2: 使用 diff3 格式的冲突

如果你想测试更完整的 diff3 格式（包含 base 部分）：

```bash
# 在你的 git 仓库中启用 diff3
git config merge.conflictstyle diff3

# 然后重复方式 1 的步骤
```

启用 diff3 后，冲突标记会包含 `||||||| base` 部分，显示共同祖先的内容。

---

### 方式 3: 代码审查验收

如果你想通过代码审查来验收，检查以下文件的修改：

#### 1. **Git Repository 接口扩展**
```bash
# 查看 Git 仓库接口的修改
git diff crates/git/src/repository.rs
git diff crates/fs/src/fake_git_repo.rs
```

**验证点：**
- ✅ `load_merge_stage_text()` 方法已添加到 trait
- ✅ 支持 stage 1/2/3 参数
- ✅ 实现使用 `index.get_path(path, stage)`

#### 2. **冲突数据模型**
```bash
git diff crates/project/src/git_store/conflict_set.rs
```

**验证点：**
- ✅ `ConflictRegion` 结构体新增三个字段：`base_text`, `ours_text`, `theirs_text`
- ✅ 添加了 `has_stage_texts()` 方法
- ✅ 添加了 `load_merge_stage_texts()` 静态方法

#### 3. **UI 组件**
```bash
# 查看新的 3-way merge 视图组件
cat crates/git_ui/src/three_way_merge_view.rs

# 查看集成到冲突视图的修改
git diff crates/git_ui/src/conflict_view.rs
git diff crates/git_ui/src/git_ui.rs
```

**验证点：**
- ✅ `ThreeWayMergeView` 组件存在
- ✅ `render_three_way_view()` 方法实现三栏布局
- ✅ `render_conflict_buttons()` 在检测到 base 时调用 3-way 视图
- ✅ 按钮操作正确调用 `resolve_conflict()`

---

### 方式 4: 单元测试（建议添加）

虽然当前实现没有专门的单元测试，但你可以运行现有的冲突解析测试：

```bash
# 运行项目相关的测试
cargo test --package project conflict

# 运行 git_ui 相关的测试
cargo test --package git_ui
```

---

## 验收清单

### 功能性验收
- [ ] Zed 能够成功编译
- [ ] 打开包含 merge 冲突的文件
- [ ] 三栏 UI 正确显示（Base / Ours / Theirs）
- [ ] 每栏显示正确的文本内容
- [ ] 背景色区分三个版本
- [ ] "Accept Base" 按钮正常工作
- [ ] "Accept Ours" 按钮正常工作
- [ ] "Accept Theirs" 按钮正常工作
- [ ] "Accept Both" 按钮正常工作
- [ ] 解决冲突后文件正确更新
- [ ] 没有 base 的简单冲突仍显示原来的 UI

### 代码质量验收
- [ ] 代码通过编译（无错误）
- [ ] 代码符合 Rust 风格指南
- [ ] 没有明显的性能问题
- [ ] Git 操作正确使用 libgit2 API
- [ ] UI 集成遵循 Zed 的现有模式

### 用户体验验收
- [ ] UI 布局清晰易懂
- [ ] 颜色对比度足够（可读性好）
- [ ] 按钮标签清晰（显示分支名）
- [ ] 操作响应及时
- [ ] 与现有冲突解决流程一致

---

## 已知限制

1. **当前实现从缓冲区解析文本**
   - 三栏显示的是从冲突标记中解析的文本
   - 未来可以直接从 Git index stages 加载更原始的版本

2. **没有逐行差异高亮**
   - 当前显示完整文本块
   - 未来可以添加字级或行级的差异标注

3. **固定高度限制**
   - 内容超过 20 行时需要滚动
   - 未来可以添加自适应高度或虚拟滚动

4. **性能未优化**
   - 大文件可能有性能问题
   - 未来可以添加延迟加载

---

## 快速验证命令

```bash
# 1. 确保编译通过
cargo check --package git_ui
cargo check --package project
cargo check --package git

# 2. 查看实现的文件
ls -l crates/git_ui/src/three_way_merge_view.rs
ls -l crates/project/src/git_store/conflict_set.rs

# 3. 搜索关键实现
grep -r "load_merge_stage_text" crates/git/src/
grep -r "ThreeWayMergeView" crates/git_ui/src/
grep -r "has_stage_texts" crates/project/src/

# 4. 运行 Zed 测试
cargo test --workspace --lib
```

---

## 反馈收集

如果在验收过程中发现问题，请记录：
- 问题描述
- 重现步骤
- 期望行为 vs 实际行为
- 错误日志（如有）
- 截图（如有）

---

## 下一步建议

验收通过后，可以考虑：
1. 添加单元测试覆盖新功能
2. 添加集成测试模拟真实合并场景
3. 性能测试大文件场景
4. 用户体验测试和迭代
5. 文档更新（用户手册）
