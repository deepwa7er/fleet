# The public face of the blog. Read-only by construction: there is no action
# here that writes, and the admin routes that do are unreachable on this host.
class PostsController < ApplicationController
  def index
    @posts = Post.published.recent
  end

  def show
    # `.published` before the lookup, not a check after it, so a draft or a
    # scheduled post is a 404 rather than a 403 — the public site should not
    # confirm that an unpublished slug exists.
    @post = Post.published.find_by!(slug: params[:id])
  end
end
