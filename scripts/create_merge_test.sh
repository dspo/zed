#!/bin/bash
# 3-Way Merge 测试环境自动创建脚本

set -e

echo "🔧 创建 3-Way Merge 测试环境..."
echo ""

# 创建测试目录
TEST_DIR="/tmp/zed-merge-test-$(date +%s)"
mkdir -p "$TEST_DIR"
cd "$TEST_DIR"

echo "📁 测试目录: $TEST_DIR"
echo ""

# 初始化 Git 仓库
git init
git config user.name "Test User"
git config user.email "test@example.com"
git config merge.conflictstyle diff3  # 启用 diff3 格式

echo "✅ Git 仓库已初始化（启用 diff3 格式）"
echo ""

# 创建初始文件
cat > README.md << 'EOF'
# Test Project

This is a test project for verifying 3-way merge functionality.

## Section 1
Original content in section 1.
This will be modified in both branches.

## Section 2
Original content in section 2.
More original content here.

## Section 3
Original content in section 3.
Final section content.
EOF

git add README.md
git commit -m "Initial commit"

echo "✅ 初始提交完成"
echo ""

# 创建 feature 分支并修改
git checkout -b feature

cat > README.md << 'EOF'
# Test Project (Feature Branch)

This is a test project for verifying 3-way merge functionality.

## Section 1
**FEATURE BRANCH CHANGE**: Updated content in section 1.
This will be modified in both branches.
Added new line in feature branch.

## Section 2
Original content in section 2.
More original content here.

## Section 3
Original content in section 3.
Final section content.
**FEATURE**: Added conclusion.
EOF

git commit -am "Feature: Update README with new content"

echo "✅ Feature 分支修改完成"
echo ""

# 切换回 main 分支并做不同修改
git checkout main

cat > README.md << 'EOF'
# Test Project (Main Branch)

This is a test project for verifying 3-way merge functionality.

## Section 1
**MAIN BRANCH CHANGE**: Different update in section 1.
This will be modified in both branches.

## Section 2
Original content in section 2.
**MAIN**: Enhanced section 2 content.
More original content here.

## Section 3
Original content in section 3.
Final section content.
EOF

git commit -am "Main: Update README with different content"

echo "✅ Main 分支修改完成"
echo ""

# 尝试合并 - 这会产生冲突
echo "🔀 尝试合并 feature 分支..."
echo ""

if git merge feature 2>&1; then
    echo "⚠️  没有产生冲突（这不应该发生）"
    exit 1
else
    echo "✅ 合并冲突已产生！"
    echo ""
fi

# 显示冲突状态
echo "📊 Git 状态:"
git status
echo ""

echo "📝 冲突文件内容预览:"
echo "================================"
head -30 README.md
echo "================================"
echo ""

echo "✨ 测试环境准备完成！"
echo ""
echo "📂 测试目录位置: $TEST_DIR"
echo ""
echo "🚀 下一步："
echo "  1. 在 Zed 中打开此目录: $TEST_DIR"
echo "  2. 打开 README.md 文件"
echo "  3. 查看 3-Way Merge UI 是否正确显示"
echo ""
echo "💡 提示："
echo "  - 左栏 (Base): 应显示原始内容"
echo "  - 中栏 (Ours): 应显示 'MAIN BRANCH CHANGE'"
echo "  - 右栏 (Theirs): 应显示 'FEATURE BRANCH CHANGE'"
echo ""

# 在 macOS 上自动打开目录（如果是 macOS）
if [[ "$OSTYPE" == "darwin"* ]]; then
    echo "📂 正在 Finder 中打开测试目录..."
    open "$TEST_DIR"
fi

echo ""
echo "✅ 完成！"
