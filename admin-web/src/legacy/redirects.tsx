import { Navigate, useParams, useLocation } from "react-router-dom";

export function RedirectWithId({ to }: { to: string }) {
  const { id } = useParams<{ id: string }>();
  return <Navigate to={id ? `${to}/${id}` : to} replace />;
}

export function RedirectWithQuery({ to }: { to: string }) {
  const location = useLocation();
  return <Navigate to={`${to}${location.search}`} replace />;
}
