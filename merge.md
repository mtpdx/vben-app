# merge

合并部分main分支内容到app分支

```bash
# 1. 切换到 app 分支并更新远端引用
git checkout app
git fetch origin

# 2. 合并 main 的最新内容到工作区，但不自动提交
git merge --no-commit --no-ff origin/main

# 3. 将要排除的文件/目录从合并前的 HEAD 恢复（保持 app 原本内容）
git restore --source=HEAD --staged --worktree playground
git restore --source=HEAD --staged --worktree .changeset
git restore --source=HEAD --staged --worktree .github
git restore --source=HEAD --staged --worktree apps/web-antd
git restore --source=HEAD --staged --worktree apps/web-ele
git restore --source=HEAD --staged --worktree apps/web-tdesign
git restore --source=HEAD --staged --worktree scripts/deploy
git restore --source=HEAD --staged --worktree .dockerignore

# 如果你有多个要排除的路径，依次写上去；也可以用通配符（注意 shell 展开）

# 4. 检查变更，确认无误后提交
git add -A
git commit -am "chore: sync merge from main (exclude ...)"

```

说明与优点：

非破坏性：合并过程中你可以检查差异后再提交。

灵活：可以临时增加/减少排除项。

可脚本化 — 放进 CI 或定期运行的脚本里。

注意点：

git restore --source=HEAD 的意思是把合并前（app 分支）的内容恢复回来，避免提交 main 的这些路径。

如果合并产生冲突，需要手动解决冲突后再执行第3步和提交。
