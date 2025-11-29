import { React } from "../../deps.ts";
const { useState, useEffect } = React;

interface SessionInfo {
  section_id: number;
  start_id: number;
  end_id: number;
  timestamp_ms: number;
  duration_seconds: number;
}

interface UseSessionNavigationProps {
  activeSession: SessionInfo | null;
  sessions: SessionInfo[];
  dateFilter: string;
  pageSize: number;
  setDateFilter: (filter: string) => void;
  setCurrentPage: (page: number) => void;
}

interface UseSessionNavigationResult {
  handleGoToActiveSession: () => void;
}

export function useSessionNavigation({
  activeSession,
  sessions,
  dateFilter,
  pageSize,
  setDateFilter,
  setCurrentPage,
}: UseSessionNavigationProps): UseSessionNavigationResult {
  const [pendingGoToSession, setPendingGoToSession] = useState<number | null>(null);

  // Scroll to the active session card
  const scrollToActiveSession = () => {
    requestAnimationFrame(() => {
      const activeCard = document.querySelector(".session-card.active");
      if (activeCard) {
        activeCard.scrollIntoView({ behavior: "smooth", block: "center" });
      }
    });
  };

  // Handle go-to-session after filter is cleared and data is loaded
  useEffect(() => {
    if (pendingGoToSession !== null && !dateFilter && sessions.length > 0) {
      const sessionIndex = sessions.findIndex(
        (s) => s.section_id === pendingGoToSession
      );
      if (sessionIndex !== -1) {
        const targetPage = Math.floor(sessionIndex / pageSize) + 1;
        setCurrentPage(targetPage);
        // Scroll after page renders
        setTimeout(scrollToActiveSession, 100);
      }
      setPendingGoToSession(null);
    }
  }, [pendingGoToSession, dateFilter, sessions, pageSize, setCurrentPage]);

  const handleGoToActiveSession = () => {
    if (!activeSession) return;
    if (dateFilter) {
      // Need to clear filter first, then navigate after data loads
      setPendingGoToSession(activeSession.section_id);
      setDateFilter("");
    } else {
      // No filter, just find and navigate to the page
      const sessionIndex = sessions.findIndex(
        (s) => s.section_id === activeSession.section_id
      );
      if (sessionIndex !== -1) {
        const targetPage = Math.floor(sessionIndex / pageSize) + 1;
        setCurrentPage(targetPage);
        // Scroll after page renders
        setTimeout(scrollToActiveSession, 100);
      }
    }
  };

  return {
    handleGoToActiveSession,
  };
}
