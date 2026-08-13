module Admin
  class PostsController < BaseController
    before_action :set_post, only: %i[show edit update destroy publish unpublish]

    def index
      # Drafts first, then everything else newest-first: the admin index is a
      # worklist, and the unfinished posts are the work.
      @drafts = Post.drafts.order(updated_at: :desc)
      @posts  = Post.where.not(published_at: nil).recent
    end

    # The author's preview. Unlike the public show this finds any post, which is
    # the whole point — it is how a draft gets read before it goes out.
    def show
    end

    def new
      @post = Post.new
    end

    def edit
    end

    def create
      @post = Post.new(post_params)

      if @post.save
        redirect_to admin_post_path(@post), notice: "Post created."
      else
        render :new, status: :unprocessable_entity
      end
    end

    def update
      if @post.update(post_params)
        redirect_to admin_post_path(@post), notice: "Post saved."
      else
        render :edit, status: :unprocessable_entity
      end
    end

    def destroy
      @post.destroy!
      redirect_to admin_root_path, notice: "Post deleted."
    end

    # Publishing is its own action rather than a checkbox on the form, so that
    # making a post public is always a deliberate, single-purpose request and
    # never a side effect of saving an edit.
    def publish
      @post.update!(published_at: Time.current)
      redirect_to admin_post_path(@post), notice: "Published."
    end

    def unpublish
      @post.update!(published_at: nil)
      redirect_to admin_post_path(@post), notice: "Returned to draft."
    end

    private

    def set_post
      @post = Post.find_by!(slug: params[:id])
    end

    # `published_at` is NOT permitted here — publishing goes through the two
    # actions above. Permitting it would let an ordinary save flip a post
    # public, which is the one state change that should never be incidental.
    # (Scheduling a future date is the one case this costs us; it is worth it.)
    def post_params
      params.expect(post: %i[title slug summary body])
    end
  end
end
