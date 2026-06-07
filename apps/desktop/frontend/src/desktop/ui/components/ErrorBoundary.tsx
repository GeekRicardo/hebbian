import { Component, type ErrorInfo, type ReactNode } from "react";
import { Toaster, toast } from "sonner";

interface Props {
  children: ReactNode;
}

interface State {
  hasError: boolean;
}

/**
 * 全局 React ErrorBoundary：捕获子树 render/commit 阶段的未处理异常，
 * 用 toast 弹出错误信息，同时尝试恢复渲染（不白屏）。
 *
 * 配合 App 里的 window error/unhandledrejection 监听，覆盖事件回调和异步代码中的异常。
 */
export class ErrorBoundary extends Component<Props, State> {
  state: State = { hasError: false };

  static getDerivedStateFromError(): State {
    return { hasError: true };
  }

  componentDidCatch(error: Error, info: ErrorInfo) {
    const message = error?.message || String(error);
    const component = info.componentStack?.split("\n")[1]?.trim() ?? "";
    console.error("[ErrorBoundary]", error, info);
    toast.error(`渲染错误: ${message}`, {
      description: component || undefined,
      duration: 12000,
    });
    // 下一帧尝试恢复——大部分瞬时渲染错误在 state 变化后可自愈
    setTimeout(() => this.setState({ hasError: false }), 0);
  }

  render() {
    if (this.state.hasError) {
      // 崩溃时渲染独立 Toaster 保证 toast 可见；
      // 同时仍渲染 children——setState(false) 后下一帧重试
      return (
        <>
          {this.props.children}
          <Toaster position="top-center" richColors closeButton toastOptions={{ className: "text-sm" }} />
        </>
      );
    }
    return this.props.children;
  }
}
