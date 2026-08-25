class CommentsController < ApplicationController
  before_action :set_comment, only: %i[edit update destroy]
  def create
    @topic = Topic.find(params[:topic_id])
    @comment = @topic.comments.build(comment_params)

    if @comment.save
      @comments = @topic.comments.order(created_at: :asc)
      respond_to do |format|
        format.turbo_stream
        format.html { redirect_to @topic, notice: "Comment added." }
      end
    else
      render "topics/show", status: :unprocessable_entity
    end
  end
  def edit
  end
  def update
    if @comment.update(comment_params)
      redirect_to @comment.topic, notice: "Comment updated."
    else
      render :edit, status: :unprocessable_entity
    end
  end
  def destroy
    topic = @comment.topic
    @comment.destroy

    respond_to do |format|
      format.turbo_stream
      format.html { redirect_to topic, notice: "Comment deleted." }
    end
    redirect_to topic, notice: "Comment deleted."
  end

  private
  def set_comment
    @comment = Comment.find(params[:id])
  end
  def comment_params
    params.expect(comment: [ :body ])
  end
end
